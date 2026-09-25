//! Supervising a vendor CLI's process, which every backend shares.
//!
//! - **Spawning** goes through `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`, so the child
//!   holds its three pipes and nothing else of wispd's, such as client sockets or the listener
//!   (#86). It leads a new session and process group, so a terminal that started wispd can't
//!   signal it, and cancelling can reach everything it started.
//! - **The environment is explicit**: a base ([`Environment::inherited`] today; #96 replaces it
//!   with one that doesn't depend on what started wispd), minus [`ALWAYS_SCRUBBED`] and the
//!   backend's scrub list, plus [`DATA_DIR_ENV`](crate::paths::DATA_DIR_ENV) from
//!   [`DataDir::command`], plus the backend's injected variables, such as an API key.
//! - **Output**: stdout as lines with a size cap, stderr into a ring buffer whose tail goes into
//!   failures, and one [`Output::Exited`] last.
//! - **Cancelling**: a signal to the CLI, then `SIGKILL` to its whole process group after a grace
//!   period. See [`CancelPolicy`].
//! - **Reaping**: a thread per child waits for it to exit, kills whatever it left running in its
//!   process group, then reaps it. Signals are never sent after the reap, so they can't reach a
//!   process that reused the pid.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitOptions, kill_process, kill_process_group, waitid,
    waitpid,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::unix::pipe;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};

use super::event::ExitInfo;
use crate::paths::DataDir;
use crate::spawn::{self, Stdio};

/// Variables no agent inherits from wispd: the SSH session that may have started it (#96).
/// Whether agents get an `SSH_AUTH_SOCK`, and which, is #96's decision.
pub const ALWAYS_SCRUBBED: &[&str] = &["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];

/// The longest stdout line a backend reads by default. Longer ones are skipped and reported. It
/// matches 0007's frame limit, which a notification built from the line has to fit anyway.
pub const DEFAULT_MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// How much of the end of stderr a process keeps by default.
pub const DEFAULT_STDERR_TAIL_BYTES: usize = 64 * 1024;

/// How long stdout may stay open after the process exited, by default. Something the CLI started
/// can hold the pipe open; this keeps it from delaying the end of the run.
pub const DEFAULT_DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);

// Sets the working directory, which the safe posix_spawn wrappers can't, then runs the program.
const TRAMPOLINE: &str = "cd -- \"$1\" && shift && exec \"$@\"";
const SHELL: &str = "/bin/sh";

/// A set of environment variables. Its `Debug` shows names only, since values can be secrets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Environment {
    vars: BTreeMap<OsString, OsString>,
}

impl Environment {
    /// No variables.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// wispd's own environment.
    #[must_use]
    pub fn inherited() -> Self {
        std::env::vars_os().collect()
    }

    /// The value of `name`.
    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.vars.get(name.as_ref()).map(OsString::as_os_str)
    }

    /// Sets `name` to `value`.
    pub fn set(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Removes `name`.
    pub fn remove(&mut self, name: impl AsRef<OsStr>) -> &mut Self {
        self.vars.remove(name.as_ref());
        self
    }

    /// The variables' names.
    pub fn names(&self) -> impl Iterator<Item = &OsStr> {
        self.vars.keys().map(OsString::as_os_str)
    }

    fn extend(&mut self, other: &Self) {
        for (name, value) in &other.vars {
            self.vars.insert(name.clone(), value.clone());
        }
    }
}

impl<K: Into<OsString>, V: Into<OsString>> FromIterator<(K, V)> for Environment {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            vars: iter
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

impl fmt::Debug for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.names()).finish()
    }
}

/// What a backend's process gets on stdin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdinMode {
    /// `/dev/null`, for CLIs that must see stdin closed, such as `codex exec`.
    Null,
    /// A pipe the backend writes to with [`Process::take_stdin`], for follow-up messages.
    Piped,
}

/// Limits on a process's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLimits {
    /// The longest stdout line, not counting the newline.
    pub max_line_bytes: usize,
    /// How much of the end of stderr to keep.
    pub stderr_tail_bytes: usize,
    /// How long stdout may stay open after the process exited.
    pub drain_after_exit: Duration,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            stderr_tail_bytes: DEFAULT_STDERR_TAIL_BYTES,
            drain_after_exit: DEFAULT_DRAIN_AFTER_EXIT,
        }
    }
}

/// A process for [`Launcher::spawn`] to start.
///
/// Secrets go in `inject`, never in `args`, which `ps` shows to every user of the machine.
#[derive(Clone, Debug)]
pub struct ProcessSpec {
    /// A program name, looked up on the child's `PATH`, or an absolute path.
    pub program: OsString,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Its working directory, which must be an absolute path to a directory.
    pub cwd: PathBuf,
    /// Variables to remove from the base environment, on top of [`ALWAYS_SCRUBBED`].
    pub scrub: Vec<OsString>,
    /// Variables to set, last, so they win over everything else.
    pub inject: Environment,
    /// What stdin is.
    pub stdin: StdinMode,
    /// Output limits.
    pub limits: OutputLimits,
}

impl ProcessSpec {
    /// A spec for `program` in `cwd` with no arguments, stdin on `/dev/null`, and default limits.
    pub fn new(program: impl Into<OsString>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            scrub: Vec::new(),
            inject: Environment::empty(),
            stdin: StdinMode::Null,
            limits: OutputLimits::default(),
        }
    }
}

/// Why a process could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The program isn't an executable file at its path, or on any directory of `PATH`.
    #[error("{} was not found; looked in {}", program.display(), display_dirs(searched))]
    NotFound {
        /// The program as given.
        program: OsString,
        /// Every place that was checked.
        searched: Vec<PathBuf>,
    },
    /// The working directory isn't an absolute path to a directory.
    #[error("the working directory {} is not usable: {reason}", cwd.display())]
    BadWorkingDirectory {
        /// The directory as given.
        cwd: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// Setting up or starting the process failed.
    #[error("could not start the process: {0}")]
    Io(#[from] io::Error),
}

fn display_dirs(dirs: &[PathBuf]) -> String {
    if dirs.is_empty() {
        return "nowhere (PATH is empty or unset)".to_owned();
    }
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Starts processes for backends, with wispd's data folder and a base environment.
#[derive(Clone, Debug)]
pub struct Launcher {
    data_dir: DataDir,
    base: Environment,
}

impl Launcher {
    /// A launcher whose processes start from `base` and reach the wispd that owns `data_dir`.
    #[must_use]
    pub fn new(data_dir: DataDir, base: Environment) -> Self {
        Self { data_dir, base }
    }

    /// The environment processes start from, before scrubbing and injection.
    #[must_use]
    pub fn base(&self) -> &Environment {
        &self.base
    }

    /// The environment `spec`'s process gets.
    #[must_use]
    pub fn environment(&self, spec: &ProcessSpec) -> Environment {
        let mut env = self.base.clone();
        for name in ALWAYS_SCRUBBED {
            env.remove(name);
        }
        for name in &spec.scrub {
            env.remove(name);
        }
        // Every spawn goes through DataDir::command (0009); only its environment is used here,
        // and injected values never enter a Command, whose Debug prints them.
        let command = self.data_dir.command(&spec.program);
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => env.set(name, value),
                None => env.remove(name),
            };
        }
        env.extend(&spec.inject);
        env
    }

    /// Starts `spec`'s process. Must be called inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// If the program or working directory is unusable, or starting the process fails.
    pub fn spawn(&self, spec: &ProcessSpec) -> Result<Process, SpawnError> {
        let env = self.environment(spec);
        check_working_directory(&spec.cwd)?;
        let program = find_program(&spec.program, env.get("PATH"))?;

        let (stdin_child, stdin_parent) = match spec.stdin {
            StdinMode::Null => (OwnedFd::from(File::open("/dev/null")?), None),
            StdinMode::Piped => {
                let (reader, writer) = io::pipe()?;
                (OwnedFd::from(reader), Some(OwnedFd::from(writer)))
            }
        };
        let (stdout_parent, stdout_child) = io::pipe()?;
        let (stderr_parent, stderr_child) = io::pipe()?;

        let argv: Vec<&OsStr> = [
            OsStr::new("sh"),
            OsStr::new("-c"),
            OsStr::new(TRAMPOLINE),
            OsStr::new("wispd-spawn"),
            spec.cwd.as_os_str(),
            program.as_os_str(),
        ]
        .into_iter()
        .chain(spec.args.iter().map(OsString::as_os_str))
        .collect();
        let pid = spawn::spawn_session(
            OsStr::new(SHELL),
            &argv,
            &env.vars,
            Stdio {
                stdin: stdin_child.as_fd(),
                stdout: stdout_child.as_fd(),
                stderr: stderr_child.as_fd(),
            },
        )?;
        drop((stdin_child, stdout_child, stderr_child));

        let shared = Arc::new(Shared {
            pid,
            reaped: Mutex::new(false),
        });
        let exited = reap_in_background(Arc::clone(&shared));
        let signals = Signals { shared };

        let pipes = (|| {
            let stdin = stdin_parent.map(pipe::Sender::from_owned_fd).transpose()?;
            let stdout = pipe::Receiver::from_owned_fd(OwnedFd::from(stdout_parent))?;
            let stderr = pipe::Receiver::from_owned_fd(OwnedFd::from(stderr_parent))?;
            io::Result::Ok((stdin, stdout, stderr))
        })();
        let (stdin, stdout, stderr) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                let _ = signals.signal_group(Signal::KILL);
                return Err(error.into());
            }
        };

        let tail = Arc::new(Mutex::new(Tail::new(spec.limits.stderr_tail_bytes)));
        let stderr_task = tokio::spawn(read_stderr(stderr, Arc::clone(&tail)));
        let (output_tx, output) = mpsc::channel(64);
        tokio::spawn(pump(
            LineReader::new(stdout, spec.limits.max_line_bytes),
            exited,
            StderrTail {
                task: stderr_task,
                tail,
            },
            output_tx,
            spec.limits.drain_after_exit,
        ));

        Ok(Process {
            signals,
            stdin,
            output,
        })
    }
}

fn check_working_directory(cwd: &Path) -> Result<(), SpawnError> {
    let bad = |reason: &str| SpawnError::BadWorkingDirectory {
        cwd: cwd.to_owned(),
        reason: reason.to_owned(),
    };
    if !cwd.is_absolute() {
        return Err(bad("it is not an absolute path"));
    }
    match fs::metadata(cwd) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(bad("it is not a directory")),
        Err(error) => Err(bad(&error.to_string())),
    }
}

/// Finds `program`: itself if it contains a `/`, which must then be absolute, or else the first
/// executable file of that name in the absolute directories of `path`.
///
/// # Errors
///
/// [`SpawnError::NotFound`], naming every place that was checked.
pub fn find_program(program: &OsStr, path: Option<&OsStr>) -> Result<PathBuf, SpawnError> {
    let not_found = |searched| SpawnError::NotFound {
        program: program.to_owned(),
        searched,
    };
    let as_path = Path::new(program);
    if program.as_encoded_bytes().contains(&b'/') {
        if as_path.is_absolute() && is_executable(as_path) {
            return Ok(as_path.to_owned());
        }
        return Err(not_found(vec![as_path.to_owned()]));
    }
    let mut searched = Vec::new();
    if !program.is_empty() {
        for dir in path.map(std::env::split_paths).into_iter().flatten() {
            if !dir.is_absolute() || searched.contains(&dir) {
                continue;
            }
            let candidate = dir.join(program);
            searched.push(dir);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(not_found(searched))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// How to stop a process when its run is cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelPolicy {
    /// The signal that asks the CLI to stop. 0004: `SIGINT` ends a Claude or Codex turn.
    pub signal: Signal,
    /// Whether that signal goes to the whole process group instead of the CLI alone. Cursor's
    /// CLI documents no cancel, so its backend signals the group.
    pub group: bool,
    /// How long to wait before `SIGKILL` goes to the whole process group.
    pub grace: Duration,
}

impl Default for CancelPolicy {
    fn default() -> Self {
        Self {
            signal: Signal::INT,
            group: false,
            grace: Duration::from_secs(10),
        }
    }
}

/// A running process: its stdin, its output, and its signals.
///
/// Dropping it kills the process's group, so a backend that goes away leaves nothing running.
#[derive(Debug)]
pub struct Process {
    signals: Signals,
    stdin: Option<pipe::Sender>,
    output: mpsc::Receiver<Output>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.signals.signal_group(Signal::KILL);
    }
}

impl Process {
    /// The process's id, which is also its process group's.
    #[must_use]
    pub fn pid(&self) -> Pid {
        self.signals.shared.pid
    }

    /// A handle that signals the process, which a backend can keep apart from the output.
    #[must_use]
    pub fn signals(&self) -> &Signals {
        &self.signals
    }

    /// The write end of stdin, if it is a pipe and hasn't been taken. Dropping it closes stdin.
    pub fn take_stdin(&mut self) -> Option<pipe::Sender> {
        self.stdin.take()
    }

    /// The next piece of output. [`Output::Exited`] always comes last, then `None`.
    pub async fn next(&mut self) -> Option<Output> {
        self.output.recv().await
    }
}

/// One piece of a process's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// A line of stdout, without its newline.
    Line(Vec<u8>),
    /// A line of stdout that was longer than the limit, which was skipped.
    Oversized {
        /// Its length, without the newline.
        bytes: usize,
    },
    /// The process exited. Nothing follows.
    Exited(Exit),
}

/// How a process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    /// Its exit code or signal.
    pub info: ExitInfo,
    /// The end of its stderr, decoded lossily, with surrounding whitespace trimmed.
    pub stderr_tail: String,
}

/// Sends signals to a process until it has been reaped, then does nothing.
#[derive(Clone, Debug)]
pub struct Signals {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    pid: Pid,
    reaped: Mutex<bool>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.reaped.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Signals {
    /// Sends `signal` to the process. Returns whether it was sent: not after the process has been
    /// reaped.
    #[must_use = "a signal is not sent once the process has been reaped"]
    pub fn signal(&self, signal: Signal) -> bool {
        let reaped = self.shared.lock();
        !*reaped && kill_process(self.shared.pid, signal).is_ok()
    }

    /// Sends `signal` to every process in the process's group, as long as the process hasn't been
    /// reaped. Returns whether it was sent.
    #[must_use = "a signal is not sent once the process has been reaped"]
    pub fn signal_group(&self, signal: Signal) -> bool {
        let reaped = self.shared.lock();
        !*reaped && kill_process_group(self.shared.pid, signal).is_ok()
    }

    /// Whether the process has exited and been reaped.
    #[must_use]
    pub fn reaped(&self) -> bool {
        *self.shared.lock()
    }

    /// Asks the process to stop with `policy`'s signal, then kills its group if it is still
    /// running after the grace period. Returns at once. Must be called inside a tokio runtime.
    pub fn cancel(&self, policy: CancelPolicy) {
        let sent = if policy.group {
            self.signal_group(policy.signal)
        } else {
            self.signal(policy.signal)
        };
        if !sent {
            return;
        }
        let signals = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(policy.grace).await;
            let _ = signals.signal_group(Signal::KILL);
        });
    }
}

/// Waits for the process on a thread of its own, which also works outside tokio and never holds
/// up a runtime's shutdown. Once the process has exited, and while its zombie still holds the
/// process group's id, whatever it left in its group is killed. Then it is reaped.
fn reap_in_background(shared: Arc<Shared>) -> oneshot::Receiver<ExitInfo> {
    let (tx, rx) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("wispd-reaper".into())
        .spawn(move || {
            let pid = shared.pid;
            let exited = loop {
                match waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
                ) {
                    Err(rustix::io::Errno::INTR) => {}
                    result => break result.is_ok(),
                }
            };
            let mut reaped = shared.lock();
            if exited {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            let info = loop {
                match waitpid(Some(pid), WaitOptions::empty()) {
                    Err(rustix::io::Errno::INTR) => {}
                    Ok(Some((_, status))) => {
                        break ExitInfo {
                            code: status.exit_status(),
                            signal: status.terminating_signal(),
                        };
                    }
                    _ => {
                        break ExitInfo {
                            code: None,
                            signal: None,
                        };
                    }
                }
            };
            *reaped = true;
            drop(reaped);
            let _ = tx.send(info);
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "could not start a thread to reap a child process");
    }
    rx
}

struct StderrTail {
    task: tokio::task::JoinHandle<()>,
    tail: Arc<Mutex<Tail>>,
}

/// Forwards stdout's lines until it ends, or until `drain` after the process exited, then sends
/// the exit last.
async fn pump<R: AsyncRead + Unpin>(
    mut lines: LineReader<R>,
    mut exited: oneshot::Receiver<ExitInfo>,
    stderr: StderrTail,
    output: mpsc::Sender<Output>,
    drain: Duration,
) {
    let unknown = ExitInfo {
        code: None,
        signal: None,
    };
    let mut exit = None;
    let mut deadline = None;
    loop {
        tokio::select! {
            biased;
            line = lines.next() => match line {
                Ok(Some(line)) => {
                    if output.send(line).await.is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => break,
            },
            info = &mut exited, if exit.is_none() => {
                exit = Some(info.unwrap_or(unknown));
                deadline = Some(Instant::now() + drain);
            }
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => break,
        }
    }
    let info = match exit {
        Some(info) => info,
        None => (&mut exited).await.unwrap_or(unknown),
    };
    let StderrTail { task, tail } = stderr;
    let _ = timeout(drain, task).await;
    let stderr_tail = tail.lock().unwrap_or_else(PoisonError::into_inner).text();
    let _ = output
        .send(Output::Exited(Exit { info, stderr_tail }))
        .await;
}

async fn read_stderr(mut stderr: pipe::Receiver, tail: Arc<Mutex<Tail>>) {
    let mut chunk = vec![0; 8192];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => tail
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(&chunk[..n]),
        }
    }
}

/// The last `capacity` bytes written to it.
#[derive(Debug)]
struct Tail {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl Tail {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity.min(8192)),
            capacity,
        }
    }

    fn push(&mut self, data: &[u8]) {
        let data = &data[data.len().saturating_sub(self.capacity)..];
        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(data);
    }

    fn text(&self) -> String {
        let (front, back) = self.bytes.as_slices();
        let mut all = Vec::with_capacity(self.bytes.len());
        all.extend_from_slice(front);
        all.extend_from_slice(back);
        String::from_utf8_lossy(&all).trim().to_owned()
    }
}

/// Splits a byte stream into lines of at most `max` bytes. A longer line is dropped as it
/// arrives, so memory stays within about `max` plus one read, and reported as
/// [`Output::Oversized`] once it ends.
#[derive(Debug)]
struct LineReader<R> {
    inner: R,
    max: usize,
    chunk: Vec<u8>,
    buf: Vec<u8>,
    scanned: usize,
    skipping: Option<usize>,
    eof: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(inner: R, max: usize) -> Self {
        Self {
            inner,
            max,
            chunk: vec![0; 64 * 1024],
            buf: Vec::new(),
            scanned: 0,
            skipping: None,
            eof: false,
        }
    }

    async fn next(&mut self) -> io::Result<Option<Output>> {
        loop {
            if let Some(offset) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
                let end = self.scanned + offset;
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                self.scanned = 0;
                if let Some(skipped) = self.skipping.take() {
                    return Ok(Some(Output::Oversized {
                        bytes: skipped + line.len(),
                    }));
                }
                if line.len() > self.max {
                    return Ok(Some(Output::Oversized { bytes: line.len() }));
                }
                return Ok(Some(Output::Line(line)));
            }
            self.scanned = self.buf.len();
            if let Some(skipped) = &mut self.skipping {
                *skipped += self.buf.len();
                self.buf.clear();
                self.scanned = 0;
            } else if self.buf.len() > self.max {
                self.skipping = Some(self.buf.len());
                self.buf.clear();
                self.scanned = 0;
            }
            if self.eof {
                if let Some(skipped) = self.skipping.take() {
                    return Ok(Some(Output::Oversized { bytes: skipped }));
                }
                if self.buf.is_empty() {
                    return Ok(None);
                }
                self.scanned = 0;
                return Ok(Some(Output::Line(std::mem::take(&mut self.buf))));
            }
            // Only complete reads change state, so a caller may drop this future at any await.
            let n = self.inner.read(&mut self.chunk).await?;
            if n == 0 {
                self.eof = true;
            }
            self.buf.extend_from_slice(&self.chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use rustix::process::{Pid, Signal};
    use tokio::io::AsyncWriteExt;

    use super::{
        CancelPolicy, Environment, Launcher, LineReader, Output, OutputLimits, Process,
        ProcessSpec, SpawnError, StdinMode, Tail, find_program,
    };
    use crate::backend::event::ExitInfo;
    use crate::paths::DataDir;

    fn launcher(base: Environment) -> Launcher {
        Launcher::new(DataDir::new("/tmp/wispd-test-data").unwrap(), base)
    }

    fn base() -> Environment {
        [("PATH", "/usr/bin:/bin")].into_iter().collect()
    }

    fn sh(script: &str) -> ProcessSpec {
        let mut spec = ProcessSpec::new("sh", "/");
        spec.args = vec!["-c".into(), script.into()];
        spec
    }

    async fn collect(process: &mut Process) -> (Vec<Output>, super::Exit) {
        let mut lines = Vec::new();
        while let Some(output) = process.next().await {
            if let Output::Exited(exit) = output {
                assert!(process.next().await.is_none(), "Exited must come last");
                return (lines, exit);
            }
            lines.push(output);
        }
        panic!("the output ended without Exited");
    }

    fn text(line: &Output) -> &str {
        match line {
            Output::Line(bytes) => std::str::from_utf8(bytes).unwrap(),
            other => panic!("expected a line, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_child_gets_an_explicit_environment() {
        let mut base = base();
        base.set("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
            .set("SSH_TTY", "/dev/ttys001")
            .set("VENDOR_TOKEN", "from-wispd")
            .set("KEPT", "yes")
            .set("OVERRIDDEN", "base");
        let mut spec = sh("/usr/bin/env | /usr/bin/sort");
        spec.scrub = vec!["VENDOR_TOKEN".into()];
        spec.inject
            .set("OVERRIDDEN", "injected")
            .set("API_KEY", "sk-secret");
        let launcher = launcher(base);
        let env = launcher.environment(&spec);
        let debug = format!("{env:?} {spec:?}");
        assert!(!debug.contains("sk-secret"), "{debug}");
        assert!(debug.contains("API_KEY"), "{debug}");

        let mut process = launcher.spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert!(exit.info.success(), "{exit:?}");
        let vars: BTreeSet<&str> = lines.iter().map(text).collect();
        for expected in [
            "API_KEY=sk-secret",
            "KEPT=yes",
            "OVERRIDDEN=injected",
            "PATH=/usr/bin:/bin",
            "WISPD_DATA_DIR=/tmp/wispd-test-data",
        ] {
            assert!(vars.contains(expected), "{expected} missing from {vars:?}");
        }
        for scrubbed in ["SSH_CONNECTION", "SSH_TTY", "VENDOR_TOKEN"] {
            assert!(
                !vars.iter().any(|v| v.starts_with(&format!("{scrubbed}="))),
                "{scrubbed} leaked: {vars:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_child_runs_in_its_working_directory_with_its_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let mut spec = ProcessSpec::new("sh", &cwd);
        spec.args = vec![
            "-c".into(),
            "pwd; printf '%s\\n' \"$@\"".into(),
            "name".into(),
            "two words".into(),
            "--flag".into(),
        ];
        let mut process = launcher(base()).spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert!(exit.info.success());
        let lines: Vec<&str> = lines.iter().map(text).collect();
        assert_eq!(lines, [cwd.to_str().unwrap(), "two words", "--flag"]);
    }

    #[tokio::test]
    async fn the_child_inherits_no_descriptor_but_its_stdio() {
        // A descriptor without close-on-exec, which a child started with std's Command would
        // inherit.
        let (_reader, writer) = std::io::pipe().unwrap();
        let leaked = rustix::io::dup(&writer).unwrap();
        assert!(leaked.as_raw_fd() < 1024);
        // Testing /dev/fd/N opens nothing, unlike ls, which opens descriptors of its own.
        let mut process = launcher(base())
            .spawn(&sh(
                "i=0; while [ $i -lt 1024 ]; do [ -e /dev/fd/$i ] && echo $i; i=$((i+1)); done",
            ))
            .unwrap();
        let (lines, exit) = collect(&mut process).await;
        drop(leaked);
        let fds: Vec<&str> = lines.iter().map(text).collect();
        assert_eq!(fds, ["0", "1", "2"], "{exit:?}");
    }

    #[tokio::test]
    async fn a_missing_program_names_where_it_was_looked_for() {
        let mut base = base();
        base.set("PATH", "/nonexistent/bin:relative:/usr/bin");
        let error = launcher(base)
            .spawn(&ProcessSpec::new("wisp-no-such-cli", "/"))
            .unwrap_err();
        let SpawnError::NotFound { searched, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(searched.len(), 2);
        assert_eq!(
            error.to_string(),
            "wisp-no-such-cli was not found; looked in /nonexistent/bin, /usr/bin"
        );
        assert!(matches!(
            find_program("relative/sh".as_ref(), None),
            Err(SpawnError::NotFound { .. })
        ));
        assert_eq!(
            find_program("sh".as_ref(), Some("/bin".as_ref())).unwrap(),
            std::path::Path::new("/bin/sh")
        );
    }

    #[tokio::test]
    async fn a_bad_working_directory_is_refused() {
        for cwd in ["relative", "/nonexistent-wisp-dir", "/bin/sh"] {
            let error = launcher(base())
                .spawn(&ProcessSpec::new("sh", cwd))
                .unwrap_err();
            assert!(
                matches!(error, SpawnError::BadWorkingDirectory { .. }),
                "{cwd}: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn stdout_lines_are_capped_and_stderr_keeps_its_tail() {
        let mut spec = sh(concat!(
            "echo short; ",
            "head -c 5000 /dev/zero | tr '\\0' x; echo; ",
            "echo after; ",
            "head -c 3000 /dev/zero | tr '\\0' e >&2; echo ' the end' >&2; ",
            "printf 'no newline'; exit 3",
        ));
        spec.limits = OutputLimits {
            max_line_bytes: 1024,
            stderr_tail_bytes: 100,
            ..OutputLimits::default()
        };
        let mut process = launcher(base()).spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert_eq!(
            lines,
            [
                Output::Line(b"short".to_vec()),
                Output::Oversized { bytes: 5000 },
                Output::Line(b"after".to_vec()),
                Output::Line(b"no newline".to_vec()),
            ]
        );
        assert_eq!(
            exit.info,
            ExitInfo {
                code: Some(3),
                signal: None
            }
        );
        assert!(exit.stderr_tail.ends_with("eee the end"), "{exit:?}");
        assert!(exit.stderr_tail.len() <= 100);
    }

    #[tokio::test]
    async fn stdin_is_a_pipe_when_asked_for() {
        let mut spec = sh("read line; echo \"got $line\"");
        spec.stdin = StdinMode::Piped;
        let mut process = launcher(base()).spawn(&spec).unwrap();
        let mut stdin = process.take_stdin().unwrap();
        stdin.write_all(b"hello\n").await.unwrap();
        drop(stdin);
        let (lines, exit) = collect(&mut process).await;
        assert!(exit.info.success());
        assert_eq!(lines, [Output::Line(b"got hello".to_vec())]);
        assert!(
            launcher(base())
                .spawn(&sh("true"))
                .unwrap()
                .take_stdin()
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancel_asks_first() {
        // A trap shows which signal arrived. Without one, bash 3.2's exit status after a SIGINT
        // in `wait` varies.
        let mut process = launcher(base())
            .spawn(&sh(
                "trap 'echo interrupted; exit 7' INT; echo ready; sleep 30 & wait $!",
            ))
            .unwrap();
        assert_eq!(process.next().await, Some(Output::Line(b"ready".to_vec())));
        let started = Instant::now();
        process.signals().cancel(CancelPolicy::default());
        let (lines, exit) = collect(&mut process).await;
        assert_eq!(lines, [Output::Line(b"interrupted".to_vec())]);
        assert_eq!(exit.info.code, Some(7), "{exit:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(process.signals().reaped());
        assert!(
            !process.signals().signal(Signal::TERM),
            "no signal after the reap"
        );
    }

    #[tokio::test]
    async fn cancel_kills_the_group_after_the_grace_period() {
        let mut process = launcher(base())
            .spawn(&sh(
                "trap '' INT; echo ready; sleep 30 & wait $!; sleep 30 & wait $!",
            ))
            .unwrap();
        assert_eq!(process.next().await, Some(Output::Line(b"ready".to_vec())));
        let started = Instant::now();
        process.signals().cancel(CancelPolicy {
            grace: Duration::from_millis(200),
            ..CancelPolicy::default()
        });
        let (_, exit) = collect(&mut process).await;
        assert_eq!(exit.info.signal, Some(Signal::KILL.as_raw()), "{exit:?}");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(5),
            "{elapsed:?}"
        );
    }

    fn alive(pid: i32) -> bool {
        rustix::process::test_kill_process(Pid::from_raw(pid).unwrap()).is_ok()
    }

    async fn wait_until_gone(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) {
            assert!(Instant::now() < deadline, "process {pid} survived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn nothing_the_child_started_outlives_it() {
        // The background sleep holds stdout open, so the exit must not wait for stdout to end.
        let mut process = launcher(base())
            .spawn(&sh("sleep 30 & echo $!; exit 0"))
            .unwrap();
        let started = Instant::now();
        let (lines, exit) = collect(&mut process).await;
        assert!(exit.info.success());
        assert!(started.elapsed() < Duration::from_secs(5));
        wait_until_gone(text(&lines[0]).parse().unwrap()).await;
    }

    #[tokio::test]
    async fn dropping_the_process_kills_it() {
        let mut process = launcher(base())
            .spawn(&sh("echo $$; sleep 30 & wait $!"))
            .unwrap();
        let Some(Output::Line(pid)) = process.next().await else {
            panic!("no pid");
        };
        let pid: i32 = String::from_utf8(pid).unwrap().parse().unwrap();
        assert_eq!(pid, process.pid().as_raw_nonzero().get());
        drop(process);
        wait_until_gone(pid).await;
    }

    #[tokio::test]
    async fn lines_split_across_reads_and_oversized_ones_are_skipped() {
        let input: &[u8] = b"one\ntwo\r\nlong-line-here\nthree";
        let mut reader = LineReader::new(input, 8);
        let mut out = Vec::new();
        while let Some(line) = reader.next().await.unwrap() {
            out.push(line);
        }
        assert_eq!(
            out,
            [
                Output::Line(b"one".to_vec()),
                Output::Line(b"two\r".to_vec()),
                Output::Oversized { bytes: 14 },
                Output::Line(b"three".to_vec()),
            ]
        );
        let mut reader = LineReader::new(&b"0123456789"[..], 4);
        assert_eq!(
            reader.next().await.unwrap(),
            Some(Output::Oversized { bytes: 10 })
        );
        assert_eq!(reader.next().await.unwrap(), None);
    }

    #[test]
    fn the_tail_keeps_the_last_bytes() {
        let mut tail = Tail::new(5);
        tail.push(b"abc");
        tail.push(b"defg");
        assert_eq!(tail.text(), "cdefg");
        tail.push(b"0123456789");
        assert_eq!(tail.text(), "56789");
    }

    #[test]
    fn environments_collect_and_hide_values() {
        let env: Environment = [(OsString::from("A"), OsString::from("1"))]
            .into_iter()
            .collect();
        assert_eq!(env.get("A"), Some("1".as_ref()));
        assert_eq!(format!("{env:?}"), "{\"A\"}");
    }
}
