//! Supervising a vendor CLI's process, which every backend shares.
//!
//! - **Spawning** goes through `posix_spawn` in `crate::spawn`, so the child holds its three
//!   pipes and nothing else of plxd's, such as client sockets or the listener (#86). It leads a
//!   new session and process group, so a terminal that started plxd can't signal it, and
//!   cancelling can reach everything it started. On Windows (0023), it goes through tokio's
//!   `Command` into a new process group and a job object of its own, which stands in for the
//!   process group below; `serve` cleared the inherit flag on all its handles at startup, so the
//!   child gets only its three pipes.
//! - **The environment is explicit**: a base (plxd's own with the usual install folders on
//!   `PATH`, decision 0014; #96 may capture the login shell's instead), minus
//!   [`ALWAYS_SCRUBBED`] and the backend's scrub list, plus
//!   [`DATA_DIR_ENV`](crate::paths::DATA_DIR_ENV) from [`DataDir::command`], plus the backend's
//!   injected variables, such as an API key.
//! - **Output**: stdout as lines with a size cap, stderr into a ring buffer whose tail goes into
//!   failures, and one [`Output::Exited`] last.
//! - **Cancelling**: a signal to the CLI, then `SIGKILL` to its whole process group after a grace
//!   period. See [`CancelPolicy`]. Windows has no signals: the backend closes stdin (Claude's
//!   does on cancel), and the job is terminated after the grace period.
//! - **Reaping**: a thread per child waits for it to exit, kills whatever it left running in its
//!   process group, then reaps it. Signals are never sent after the reap, so they can't reach a
//!   process that reused the pid. On Windows, a task waits for it and terminates its job.
//!
//! **Limit:** a process that leaves the group, with `setsid` or `setpgid`, escapes all of this.
//! macOS has no way to follow it short of scanning the process table, and plxd doesn't use
//! Linux's ways (a child subreaper or a cgroup). It is reparented to launchd or init, which reaps
//! it; plxd never waits for it. It can't hold a run open either: once the CLI exits, stdout is
//! cut off after [`OutputLimits::drain_after_exit`], though never before what the CLI itself wrote
//! has been read, and stdin writes stop at a timeout.
//! Daemons an agent starts on purpose, such as a dev server, therefore outlive the run.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

#[cfg(unix)]
pub use rustix::process::Signal;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use zeroize::Zeroize;

use super::event::ExitInfo;
use crate::paths::DataDir;

/// The write end of a process's stdin.
#[cfg(unix)]
pub type StdinPipe = tokio::net::unix::pipe::Sender;
/// The write end of a process's stdin.
#[cfg(windows)]
pub type StdinPipe = tokio::process::ChildStdin;

/// What [`Signals`] can send. Windows has no signals, so there `INT` and `TERM` send nothing (the
/// backend closes stdin instead) and `KILL` terminates the process's job (0023). The numbers are
/// POSIX's, for [`ExitInfo`].
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signal(i32);

#[cfg(windows)]
impl Signal {
    /// Asks the CLI to stop: nothing is sent on Windows.
    pub const INT: Self = Self(2);
    /// Asks the CLI to stop: nothing is sent on Windows.
    pub const TERM: Self = Self(15);
    /// Kills the process's job.
    pub const KILL: Self = Self(9);

    /// The POSIX number.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self.0
    }
}

/// Variables no process plxd starts inherits: the SSH session that may have started it (#96).
/// That includes its `SSH_AUTH_SOCK`, which stops working when the session ends and which no
/// agent needs: workers can't push, and plxd makes every commit locally (decision 0014).
pub const ALWAYS_SCRUBBED: &[&str] = &["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK"];

/// The longest stdout line a backend reads by default. Longer ones are skipped and reported. It
/// matches 0007's frame limit, which a notification built from the line has to fit anyway.
pub const DEFAULT_MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// How much of the end of stderr a process keeps by default.
pub const DEFAULT_STDERR_TAIL_BYTES: usize = 64 * 1024;

/// How long stdout may stay open after the process exited, by default. Something the CLI started
/// can hold the pipe open; this keeps it from delaying the end of the run. A stdout that is still
/// busy is cut only once the drain has passed and more than `READ_BEFORE_CUT` has been read since
/// the exit.
pub const DEFAULT_DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);

/// How much stdout [`LineReader`] asks for at a time.
const READ_CHUNK: usize = 64 * 1024;

/// How much stdout is read after the exit before the drain may cut off a stdout that is still
/// ready. What the process wrote before it exited is unread then: at most a pipe's worth (64 KiB
/// by default on macOS and Linux, and up to Linux's default `pipe-max-size`, 1 MiB, if the
/// process grows its pipe; Windows adds tokio's 64 KiB read-ahead) plus one read. A slow reader
/// can take longer than the drain to get through it (RYA-140).
const READ_BEFORE_CUT: u64 = 1024 * 1024 + READ_CHUNK as u64;

// Sets the working directory, which the safe posix_spawn wrappers can't, then runs the program.
#[cfg(unix)]
const TRAMPOLINE: &str = "cd -- \"$1\" && shift && exec \"$@\"";
#[cfg(unix)]
const SHELL: &str = "/bin/sh";

/// A set of environment variables. Its `Debug` shows names only, since values can be secrets, and
/// a value it drops or replaces (`set`, `remove`, going out of scope) is zeroized first, so an
/// API key (#118) doesn't sit in a freed allocation.
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

    /// plxd's own environment. Windows' variable names are case-insensitive, and some come
    /// spelled like `Path`, so there every name is upper-cased.
    #[must_use]
    pub fn inherited() -> Self {
        std::env::vars_os()
            .map(|(name, value)| {
                let name = if cfg!(windows) {
                    name.to_ascii_uppercase()
                } else {
                    name
                };
                (name, value)
            })
            .collect()
    }

    /// The value of `name`.
    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.vars.get(name.as_ref()).map(OsString::as_os_str)
    }

    /// Sets `name` to `value`, zeroizing whatever value `name` had before.
    pub fn set(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        zeroize_os_string(self.vars.insert(name.into(), value.into()));
        self
    }

    /// Removes `name`, zeroizing its value.
    pub fn remove(&mut self, name: impl AsRef<OsStr>) -> &mut Self {
        zeroize_os_string(self.vars.remove(name.as_ref()));
        self
    }

    /// The variables' names.
    pub fn names(&self) -> impl Iterator<Item = &OsStr> {
        self.vars.keys().map(OsString::as_os_str)
    }

    fn extend(&mut self, other: &Self) {
        for (name, value) in &other.vars {
            zeroize_os_string(self.vars.insert(name.clone(), value.clone()));
        }
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        for value in self.vars.values_mut() {
            std::mem::take(value).into_encoded_bytes().zeroize();
        }
    }
}

/// Zeroizes `value`'s bytes before it is freed, if there is one.
fn zeroize_os_string(value: Option<OsString>) {
    if let Some(value) = value {
        value.into_encoded_bytes().zeroize();
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
    /// `/dev/null` (`NUL` on Windows), for CLIs that must see stdin closed, such as `codex exec`.
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
    /// How long stdout may stay open after the process exited. A stdout that is still busy is cut
    /// only once this has passed and more than `READ_BEFORE_CUT` has been read since the exit.
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
    /// Whether stderr's lines also come as [`Output::Line`]s while the process runs, mixed in
    /// with stdout's, for a CLI that prints what plxd reads there, such as `gh auth login`'s
    /// one-time code. They still go into [`Exit::stderr_tail`] too.
    pub stderr_lines: bool,
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
            stderr_lines: false,
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
    /// On Windows, the program is a batch file, such as an npm `.cmd` shim, and std refused an
    /// argument it can't pass to one safely (0023).
    #[error(
        "{} is a batch file, which can't be given one of plxd's arguments safely ({source}); \
         install the CLI's native build instead of its npm package",
        program.display()
    )]
    BatchFile {
        /// The batch file.
        program: PathBuf,
        /// std's refusal.
        #[source]
        source: io::Error,
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

/// Starts processes for backends, with plxd's data folder and a base environment.
#[derive(Clone, Debug)]
pub struct Launcher {
    data_dir: DataDir,
    base: Environment,
}

impl Launcher {
    /// A launcher whose processes start from `base` and reach the plxd that owns `data_dir`.
    #[must_use]
    pub fn new(data_dir: DataDir, base: Environment) -> Self {
        Self { data_dir, base }
    }

    /// The environment processes start from, before scrubbing and injection.
    #[must_use]
    pub fn base(&self) -> &Environment {
        &self.base
    }

    /// plxd's data folder.
    #[must_use]
    pub fn data_dir(&self) -> &DataDir {
        &self.data_dir
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
        let Started {
            signals,
            exited,
            stdin,
            stdout,
            stderr,
        } = start(spec, &program, &env)?;

        let tail = Arc::new(Mutex::new(Tail::new(spec.limits.stderr_tail_bytes)));
        let (output_tx, output) = mpsc::channel(64);
        let lines = spec
            .stderr_lines
            .then(|| (output_tx.clone(), spec.limits.max_line_bytes));
        let stderr_task = tokio::spawn(read_stderr(stderr, Arc::clone(&tail), lines));
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

/// A process [`start`] started, and its pipes.
struct Started<O, E> {
    signals: Signals,
    exited: oneshot::Receiver<ExitInfo>,
    stdin: Option<StdinPipe>,
    stdout: O,
    stderr: E,
}

/// Starts `program` for `spec` with exactly `env`, through `posix_spawn` and the `sh` trampoline
/// that sets its working directory.
#[cfg(unix)]
fn start(
    spec: &ProcessSpec,
    program: &Path,
    env: &Environment,
) -> Result<Started<tokio::net::unix::pipe::Receiver, tokio::net::unix::pipe::Receiver>, SpawnError>
{
    use std::os::fd::{AsFd as _, OwnedFd};

    use tokio::net::unix::pipe;

    let (stdin_child, stdin_parent) = match spec.stdin {
        StdinMode::Null => (OwnedFd::from(fs::File::open("/dev/null")?), None),
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
        OsStr::new("plxd-spawn"),
        spec.cwd.as_os_str(),
        program.as_os_str(),
    ]
    .into_iter()
    .chain(spec.args.iter().map(OsString::as_os_str))
    .collect();
    let pid = crate::spawn::spawn_session(
        OsStr::new(SHELL),
        &argv,
        &env.vars,
        crate::spawn::Stdio {
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
    match pipes {
        Ok((stdin, stdout, stderr)) => Ok(Started {
            signals,
            exited,
            stdin,
            stdout,
            stderr,
        }),
        Err(error) => {
            let _ = signals.signal_group(Signal::KILL);
            Err(error.into())
        }
    }
}

/// Starts `program` for `spec` with exactly `env`, in a new process group with no window, and
/// puts it in a job of its own (0023). A batch file, such as an npm `.cmd` shim, runs through
/// std's escaping for `cmd.exe`, which refuses an argument it can't pass safely.
///
/// std's `Command` keeps its own copy of `env` until it drops, which isn't zeroized the way
/// Unix's copies are.
#[cfg(windows)]
fn start(
    spec: &ProcessSpec,
    program: &Path,
    env: &Environment,
) -> Result<Started<tokio::process::ChildStdout, tokio::process::ChildStderr>, SpawnError> {
    use std::process::Stdio;

    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    let job = crate::windows::Job::new()?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&env.vars)
        .stdin(match spec.stdin {
            StdinMode::Null => Stdio::null(),
            StdinMode::Piped => Stdio::piped(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|source| {
        let batch = program.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });
        if batch && source.kind() == io::ErrorKind::InvalidInput {
            SpawnError::BatchFile {
                program: program.to_owned(),
                source,
            }
        } else {
            SpawnError::Io(source)
        }
    })?;
    drop(command);
    // ponytail: a process the CLI starts before it joins the job escapes it. That window is the
    // few microseconds before this call; CREATE_SUSPENDED and resuming its thread would close it.
    if let Err(error) = job.assign(&child) {
        let _ = child.start_kill();
        return Err(error.into());
    }
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let shared = Arc::new(Shared {
        pid: child.id().unwrap_or(0),
        job,
        reaped: Mutex::new(false),
    });
    let exited = reap_in_background(Arc::clone(&shared), child);
    match (stdout, stderr) {
        (Some(stdout), Some(stderr)) => Ok(Started {
            signals: Signals { shared },
            exited,
            stdin,
            stdout,
            stderr,
        }),
        _ => Err(io::Error::other("the process's pipes are missing").into()),
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

/// Finds `program`: itself if it contains a `/` (or on Windows a `\\`), which must then be
/// absolute, or else the first executable file of that name in the absolute directories of
/// `path`. On Windows, a name without an extension is tried with each of `PATHEXT`'s, such as
/// `.exe` and `.cmd`, as Windows' own lookup does (0023).
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
    let bytes = program.as_encoded_bytes();
    if bytes.contains(&b'/') || (cfg!(windows) && bytes.contains(&b'\\')) {
        if as_path.is_absolute()
            && let Some(found) = executable(as_path)
        {
            return Ok(found);
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
            if let Some(found) = executable(&candidate) {
                return Ok(found);
            }
        }
    }
    Err(not_found(searched))
}

/// `path`, if it is an executable file.
#[cfg(unix)]
fn executable(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .then(|| path.to_owned())
}

/// `path` if it names a file with an extension, or else the first of `path` plus each of
/// `PATHEXT`'s extensions that is a file.
#[cfg(windows)]
fn executable(path: &Path) -> Option<PathBuf> {
    if path.extension().is_some() {
        return path.is_file().then(|| path.to_owned());
    }
    let extensions = std::env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    extensions
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| {
            let mut candidate = path.as_os_str().to_owned();
            candidate.push(extension);
            PathBuf::from(candidate)
        })
        .find(|candidate| candidate.is_file())
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
/// Dropping it kills the process's group (on Windows, its job), so a backend that goes away
/// leaves nothing running.
#[derive(Debug)]
pub struct Process {
    signals: Signals,
    stdin: Option<StdinPipe>,
    output: mpsc::Receiver<Output>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.signals.signal_group(Signal::KILL);
    }
}

impl Process {
    /// The process's id, which is also its process group's.
    #[cfg(unix)]
    #[must_use]
    pub fn pid(&self) -> rustix::process::Pid {
        self.signals.shared.pid
    }

    /// The process's id.
    #[cfg(windows)]
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.signals.shared.pid
    }

    /// A handle that signals the process, which a backend can keep apart from the output.
    #[must_use]
    pub fn signals(&self) -> &Signals {
        &self.signals
    }

    /// The write end of stdin, if it is a pipe and hasn't been taken. Dropping it closes stdin.
    pub fn take_stdin(&mut self) -> Option<StdinPipe> {
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
    #[cfg(unix)]
    pid: rustix::process::Pid,
    #[cfg(windows)]
    pid: u32,
    /// The job the process runs in, which stands in for its process group.
    #[cfg(windows)]
    job: crate::windows::Job,
    reaped: Mutex<bool>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.reaped.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends `signal` to the process, or its whole group. The caller holds the lock and has
    /// checked that the process hasn't been reaped.
    #[cfg(unix)]
    fn send(&self, signal: Signal, group: bool) -> bool {
        if group {
            rustix::process::kill_process_group(self.pid, signal).is_ok()
        } else {
            rustix::process::kill_process(self.pid, signal).is_ok()
        }
    }

    /// `KILL` terminates the job, to the process alone or its group alike; `INT` and `TERM`
    /// have nothing to send on Windows, and count as sent.
    #[cfg(windows)]
    fn send(&self, signal: Signal, _group: bool) -> bool {
        signal != Signal::KILL || self.job.terminate().is_ok()
    }
}

impl Signals {
    /// Sends `signal` to the process. Returns whether it was sent: not after the process has been
    /// reaped.
    #[must_use = "a signal is not sent once the process has been reaped"]
    pub fn signal(&self, signal: Signal) -> bool {
        let reaped = self.shared.lock();
        !*reaped && self.shared.send(signal, false)
    }

    /// Sends `signal` to every process in the process's group, as long as the process hasn't been
    /// reaped. Returns whether it was sent.
    #[must_use = "a signal is not sent once the process has been reaped"]
    pub fn signal_group(&self, signal: Signal) -> bool {
        let reaped = self.shared.lock();
        !*reaped && self.shared.send(signal, true)
    }

    /// Whether the process has exited and been reaped.
    #[must_use]
    pub fn reaped(&self) -> bool {
        *self.shared.lock()
    }

    /// Asks the process to stop with `policy`'s signal, then kills its group if it is still
    /// running after the grace period. Returns at once, from any thread.
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
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(policy.grace).await;
                let _ = signals.signal_group(Signal::KILL);
            });
        } else {
            let _ = std::thread::Builder::new()
                .name("plxd-cancel".into())
                .spawn(move || {
                    std::thread::sleep(policy.grace);
                    let _ = signals.signal_group(Signal::KILL);
                });
        }
    }
}

/// Waits for the process on a task, then terminates its job, which kills whatever it left
/// running, before it counts as reaped.
#[cfg(windows)]
fn reap_in_background(
    shared: Arc<Shared>,
    mut child: tokio::process::Child,
) -> oneshot::Receiver<ExitInfo> {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let status = child.wait().await;
        let mut reaped = shared.lock();
        let _ = shared.job.terminate();
        *reaped = true;
        drop(reaped);
        let _ = tx.send(ExitInfo {
            code: status.ok().and_then(|status| status.code()),
            signal: None,
        });
    });
    rx
}

/// Waits for the process on a thread of its own, which also works outside tokio and never holds
/// up a runtime's shutdown. Once the process has exited, and while its zombie still holds the
/// process group's id, whatever it left in its group is killed. Then it is reaped.
#[cfg(unix)]
fn reap_in_background(shared: Arc<Shared>) -> oneshot::Receiver<ExitInfo> {
    use rustix::process::{
        WaitId, WaitIdOptions, WaitOptions, kill_process_group, waitid, waitpid,
    };

    let (tx, rx) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("plxd-reaper".into())
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

/// Forwards stdout's lines until it ends, then sends the exit last. After the process exits,
/// stdout is cut off once `drain` has passed and it either has nothing ready or has had more than
/// [`READ_BEFORE_CUT`] read since the exit.
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
    // How much stdout had been read when the exit was seen.
    let mut read_at_exit = 0;
    loop {
        tokio::select! {
            biased;
            line = lines.next() => match line {
                Ok(Some(line)) => {
                    if output.send(line).await.is_err() {
                        return;
                    }
                    // Something outside the process group may keep writing after the exit. With
                    // `biased`, a stdout that is always ready would keep the other arms from
                    // ever seeing the exit or the deadline, so check both here too, once the
                    // process's own output has been read.
                    if exit.is_none()
                        && let Ok(info) = exited.try_recv()
                    {
                        exit = Some(info);
                        deadline = Some(Instant::now() + drain);
                        read_at_exit = lines.read;
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline)
                        && lines.read - read_at_exit > READ_BEFORE_CUT
                    {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            },
            info = &mut exited, if exit.is_none() => {
                exit = Some(info.unwrap_or(unknown));
                deadline = Some(Instant::now() + drain);
                read_at_exit = lines.read;
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

/// Keeps stderr's tail, and with `lines` also sends each line of it up to the size limit, without
/// its newline. [`pump`] waits for this before it sends the exit, so the lines come first.
async fn read_stderr<R: AsyncRead + Unpin>(
    mut stderr: R,
    tail: Arc<Mutex<Tail>>,
    lines: Option<(mpsc::Sender<Output>, usize)>,
) {
    let mut chunk = vec![0; 8192];
    let mut line = Vec::new();
    loop {
        let n = match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        tail.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(&chunk[..n]);
        let Some((output, max)) = &lines else {
            continue;
        };
        line.extend_from_slice(&chunk[..n]);
        while let Some(end) = line.iter().position(|&b| b == b'\n') {
            let rest = line.split_off(end + 1);
            let mut done = std::mem::replace(&mut line, rest);
            done.pop();
            if done.last() == Some(&b'\r') {
                done.pop();
            }
            if done.len() <= *max && output.send(Output::Line(done)).await.is_err() {
                return;
            }
        }
        if line.len() > *max {
            line.clear();
        }
    }
    if let Some((output, _)) = lines
        && !line.is_empty()
    {
        let _ = output.send(Output::Line(line)).await;
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
///
/// Consumed lines only advance `start`; the buffer is compacted once per read, not per line.
#[derive(Debug)]
struct LineReader<R> {
    inner: R,
    max: usize,
    chunk: Vec<u8>,
    buf: Vec<u8>,
    start: usize,
    scanned: usize,
    skipping: Option<usize>,
    eof: bool,
    /// Bytes read from `inner` so far.
    read: u64,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(inner: R, max: usize) -> Self {
        Self {
            inner,
            max,
            chunk: vec![0; READ_CHUNK],
            buf: Vec::new(),
            start: 0,
            scanned: 0,
            skipping: None,
            eof: false,
            read: 0,
        }
    }

    fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    fn discard_pending(&mut self) {
        self.buf.clear();
        self.start = 0;
        self.scanned = 0;
    }

    async fn next(&mut self) -> io::Result<Option<Output>> {
        loop {
            let from = self.scanned.max(self.start);
            if let Some(offset) = self.buf[from..].iter().position(|&b| b == b'\n') {
                let end = from + offset;
                let length = end - self.start;
                let line = &self.buf[self.start..end];
                let output = if let Some(skipped) = self.skipping.take() {
                    Output::Oversized {
                        bytes: skipped + length,
                    }
                } else if length > self.max {
                    Output::Oversized { bytes: length }
                } else {
                    Output::Line(line.to_vec())
                };
                self.start = end + 1;
                self.scanned = self.start;
                return Ok(Some(output));
            }
            self.scanned = self.buf.len();
            if let Some(skipped) = &mut self.skipping {
                *skipped += self.buf.len() - self.start;
                self.discard_pending();
            } else if self.pending() > self.max {
                self.skipping = Some(self.pending());
                self.discard_pending();
            }
            if self.eof {
                if let Some(skipped) = self.skipping.take() {
                    return Ok(Some(Output::Oversized { bytes: skipped }));
                }
                if self.pending() == 0 {
                    return Ok(None);
                }
                let line = self.buf[self.start..].to_vec();
                self.discard_pending();
                return Ok(Some(Output::Line(line)));
            }
            // Only complete reads change state, so a caller may drop this future at any await.
            let n = self.inner.read(&mut self.chunk).await?;
            if self.start > 0 {
                self.buf.drain(..self.start);
                self.scanned -= self.start;
                self.start = 0;
            }
            if n == 0 {
                self.eof = true;
            }
            self.read += n as u64;
            self.buf.extend_from_slice(&self.chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::fd::AsRawFd;
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::Instant;

    #[cfg(unix)]
    use rustix::process::{Pid, Signal};
    #[cfg(unix)]
    use tokio::io::AsyncWriteExt;

    #[cfg(unix)]
    use super::{
        CancelPolicy, Launcher, OutputLimits, Process, ProcessSpec, SpawnError, StdinMode,
        find_program,
    };
    use super::{Environment, LineReader, Output, Tail};
    use crate::backend::event::ExitInfo;
    #[cfg(unix)]
    use crate::paths::DataDir;

    #[cfg(unix)]
    fn launcher(base: Environment) -> Launcher {
        Launcher::new(DataDir::new("/tmp/plxd-test-data").unwrap(), base)
    }

    #[cfg(unix)]
    fn base() -> Environment {
        [("PATH", "/usr/bin:/bin")].into_iter().collect()
    }

    #[cfg(unix)]
    fn sh(script: &str) -> ProcessSpec {
        let mut spec = ProcessSpec::new("sh", "/");
        spec.args = vec!["-c".into(), script.into()];
        spec
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    fn text(line: &Output) -> &str {
        match line {
            Output::Line(bytes) => std::str::from_utf8(bytes).unwrap(),
            other => panic!("expected a line, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_gets_an_explicit_environment() {
        let mut base = base();
        base.set("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
            .set("SSH_TTY", "/dev/ttys001")
            .set("VENDOR_TOKEN", "from-plxd")
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
            "PLXD_DATA_DIR=/tmp/plxd-test-data",
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_inherits_no_descriptor_but_its_stdio() {
        // A descriptor without close-on-exec, which a child started with std's Command would
        // inherit.
        let (_reader, writer) = std::io::pipe().unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the test needs a leaked descriptor"
        )]
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_program_names_where_it_was_looked_for() {
        let mut base = base();
        base.set("PATH", "/nonexistent/bin:relative:/usr/bin");
        let error = launcher(base)
            .spawn(&ProcessSpec::new("parallax-no-such-cli", "/"))
            .unwrap_err();
        let SpawnError::NotFound { searched, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(searched.len(), 2);
        assert_eq!(
            error.to_string(),
            "parallax-no-such-cli was not found; looked in /nonexistent/bin, /usr/bin"
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_bad_working_directory_is_refused() {
        for cwd in ["relative", "/nonexistent-parallax-dir", "/bin/sh"] {
            let error = launcher(base())
                .spawn(&ProcessSpec::new("sh", cwd))
                .unwrap_err();
            assert!(
                matches!(error, SpawnError::BadWorkingDirectory { .. }),
                "{cwd}: {error:?}"
            );
        }
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stderr_lines_come_as_lines_when_asked_for() {
        let mut spec = sh("printf 'code\\r\\n' >&2; printf 'last' >&2");
        spec.stderr_lines = true;
        let mut process = launcher(base()).spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert_eq!(
            lines,
            [
                Output::Line(b"code".to_vec()),
                Output::Line(b"last".to_vec())
            ]
        );
        assert_eq!(exit.stderr_tail, "code\r\nlast");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_asks_first() {
        // A trap shows which signal arrived. Without one, bash 3.2's exit status after a SIGINT
        // in `wait` varies. Printing "ready" only after the trap is installed is this test's
        // deterministic handshake: reading that line can never race the trap's own installation.
        let mut process = launcher(base())
            .spawn(&sh(
                "trap 'echo interrupted; exit 7' INT; echo ready; sleep 30 & wait $!",
            ))
            .unwrap();
        assert_eq!(process.next().await, Some(Output::Line(b"ready".to_vec())));
        let started = Instant::now();
        // Signals the process directly instead of `cancel()`, which would also arm the
        // grace-then-`SIGKILL` escalation. This test is about the trap's own reaction to
        // `SIGINT`, not the escalation (`cancel_kills_the_group_after_the_grace_period` owns
        // that), so it never arms a second timer that could race the trap's clean exit (#139).
        assert!(process.signals().signal(Signal::INT));
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    fn alive(pid: i32) -> bool {
        rustix::process::test_kill_process(Pid::from_raw(pid).unwrap()).is_ok()
    }

    #[cfg(unix)]
    async fn wait_until_gone(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) {
            assert!(Instant::now() < deadline, "process {pid} survived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
        let many = b"a\nbb\n\nccc\n".repeat(10_000);
        let mut reader = LineReader::new(&many[..], 8);
        let mut count = 0;
        while let Some(line) = reader.next().await.unwrap() {
            assert!(matches!(line, Output::Line(_)), "{line:?}");
            count += 1;
        }
        assert_eq!(count, 40_000);
        let mut reader = LineReader::new(&b"0123456789"[..], 4);
        assert_eq!(
            reader.next().await.unwrap(),
            Some(Output::Oversized { bytes: 10 })
        );
        assert_eq!(reader.next().await.unwrap(), None);
    }

    /// Pumps `stdout` for a process that has already exited, with a drain of `drain`.
    fn pump_exited(
        stdout: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        drain: Duration,
    ) -> tokio::sync::mpsc::Receiver<Output> {
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        exit_tx
            .send(ExitInfo {
                code: Some(0),
                signal: None,
            })
            .unwrap();
        let stderr = super::StderrTail {
            task: tokio::spawn(async {}),
            tail: std::sync::Arc::new(std::sync::Mutex::new(Tail::new(16))),
        };
        let (output_tx, output) = tokio::sync::mpsc::channel(64);
        tokio::spawn(super::pump(
            LineReader::new(stdout, 1024),
            exit_rx,
            stderr,
            output_tx,
            drain,
        ));
        output
    }

    #[tokio::test]
    async fn the_drain_never_drops_what_the_process_wrote() {
        // More than one read's worth, left unread at the exit, in a pipe something else keeps
        // open. However long the reader takes, all of it comes through (RYA-140).
        let (mut writer, reader) = tokio::io::duplex(1024 * 1024);
        let written: Vec<String> = (0..20_000).map(|n| format!("line {n}")).collect();
        let text = format!("{}\n", written.join("\n"));
        tokio::io::AsyncWriteExt::write_all(&mut writer, text.as_bytes())
            .await
            .unwrap();
        let mut output = pump_exited(reader, Duration::ZERO);
        let mut lines = Vec::new();
        while let Some(Output::Line(line)) = output.recv().await {
            lines.push(String::from_utf8(line).unwrap());
        }
        assert_eq!(lines, written);
        drop(writer);
    }

    #[tokio::test]
    async fn stdout_that_never_ends_cannot_hold_the_exit_back() {
        // Like a process outside the group that keeps writing after the CLI exited. It is cut
        // off after `READ_BEFORE_CUT`, a million empty lines, which takes a second in a debug
        // build; the timeout only catches it never ending.
        let mut output = pump_exited(tokio::io::repeat(b'\n'), Duration::from_millis(100));
        let exit = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(Output::Exited(exit)) = output.recv().await {
                    return exit;
                }
            }
        })
        .await
        .expect("the exit never came");
        assert!(exit.info.success());
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

#[cfg(all(test, windows))]
mod windows_tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::{
        CancelPolicy, Environment, Exit, Launcher, Output, Process, ProcessSpec, Signal,
        SpawnError, find_program,
    };
    use crate::paths::DataDir;

    const PATIENCE: Duration = Duration::from_secs(10);

    fn launcher(base: Environment) -> Launcher {
        let data = std::env::temp_dir().join("plxd-test-data");
        Launcher::new(DataDir::new(data).unwrap(), base)
    }

    fn cmd(script: &str) -> ProcessSpec {
        let mut spec = ProcessSpec::new("cmd", std::env::temp_dir());
        spec.args = vec!["/c".into(), script.into()];
        spec
    }

    async fn collect(process: &mut Process) -> (Vec<String>, Exit) {
        let mut lines = Vec::new();
        loop {
            match timeout(PATIENCE, process.next()).await.expect("output") {
                Some(Output::Line(line)) => {
                    lines.push(String::from_utf8(line).unwrap().trim_end().to_owned());
                }
                Some(Output::Exited(exit)) => return (lines, exit),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_child_gets_its_environment_working_directory_and_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = cmd("echo %PLX_TEST%& cd& exit 3");
        spec.cwd = dir.path().to_owned();
        spec.inject.set("PLX_TEST", "set");
        let mut process = launcher(Environment::inherited()).spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert_eq!(exit.info.code, Some(3), "{exit:?}");
        assert_eq!(lines[0], "set");
        assert!(
            lines[1].eq_ignore_ascii_case(&dir.path().display().to_string()),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn cancelling_terminates_the_job() {
        let spec = cmd("ping -n 30 127.0.0.1");
        let mut process = launcher(Environment::inherited()).spawn(&spec).unwrap();
        process.signals().cancel(CancelPolicy {
            signal: Signal::INT,
            group: false,
            grace: Duration::ZERO,
        });
        let (_, exit) = collect(&mut process).await;
        assert_ne!(exit.info.code, Some(0), "{exit:?}");
        assert!(process.signals().reaped());
    }

    #[tokio::test]
    async fn a_cmd_shim_is_found_and_an_argument_it_cant_take_names_the_native_build() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.cmd"), "@echo %~1\r\n").unwrap();
        let found = find_program("tool".as_ref(), Some(dir.path().as_os_str())).unwrap();
        assert!(
            found
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd")),
            "{found:?}"
        );

        let mut base = Environment::inherited();
        base.set("PATH", dir.path());
        let launcher = launcher(base);
        let mut spec = ProcessSpec::new("tool", dir.path());
        spec.args = vec!["hello".into()];
        let mut process = launcher.spawn(&spec).unwrap();
        let (lines, exit) = collect(&mut process).await;
        assert_eq!(exit.info.code, Some(0), "{exit:?}");
        assert_eq!(lines, ["hello"]);

        spec.args = vec!["two\nlines".into()];
        match launcher.spawn(&spec) {
            Err(error @ SpawnError::BatchFile { .. }) => {
                assert!(error.to_string().contains("native build"), "{error}");
            }
            other => panic!("expected BatchFile, got {other:?}"),
        }
    }
}
