//! Running `wispd attach`, and the `wispd serve` it reaches, in temporary data folders.
//!
//! Every data folder is under `/tmp`, which keeps socket paths under macOS's 103-byte limit and
//! keeps the tests away from the real data folder and any installed launch agent.

use std::fmt;
use std::fs::{self, File};
use std::io::{PipeReader, PipeWriter, Read};
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use rustix::process::{Pid, Signal, WaitOptions};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, sleep, timeout};
use tokio_util::codec::FramedRead;
use wisp_protocol::framing::FrameCodec;
use wisp_protocol::jsonrpc::{ErrorObject, Message, Request, RequestId, Response};
use wisp_protocol::methods::{Initialize, RequestMethod};
use wisp_protocol::{
    Capabilities, ClientInfo, InitializeParams, InitializeResult, ProjectCreateParams, ProjectId,
    ProtocolRange,
};
use wispd::paths::DataDir;

/// How long a test waits for anything before it fails.
pub const PATIENCE: Duration = Duration::from_secs(10);

pub const WISPD: &str = env!("CARGO_BIN_EXE_wispd");

/// A fresh folder under `/tmp`, removed when dropped.
pub fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("wispd-")
        .tempdir_in("/tmp")
        .expect("create a temp dir under /tmp")
}

/// Held while a test spawns a process or makes a descriptor without close-on-exec.
///
/// macOS can't create a pipe or socket with close-on-exec already set, so a process that one test
/// spawns can inherit a descriptor that another test is creating at that moment (#86). A test
/// that waits for a pipe to close would then wait for a stranger to exit.
pub fn spawn_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    DataDir::new(data_dir)
        .expect("resolve the data folder")
        .socket_path()
        .expect("find the socket path")
        .path
}

pub fn log(data_dir: &Path) -> String {
    fs::read_to_string(data_dir.join("logs/wispd.log")).unwrap_or_default()
}

/// The pid in the lock file, which belongs to the `serve` running for `data_dir`.
pub fn serve_pid(data_dir: &Path) -> Option<Pid> {
    let text = fs::read_to_string(data_dir.join("wispd.lock")).ok()?;
    Pid::from_raw(text.trim().parse().ok()?)
}

/// Whether `pid` still runs. A test that called [`wispd::attach::connect`] itself is the parent
/// of the `serve` it started, so that one is reaped here once it exits.
pub fn is_alive(pid: Pid) -> bool {
    let _ = rustix::process::waitpid(Some(pid), WaitOptions::NOHANG);
    rustix::process::test_kill_process(pid).is_ok()
}

/// Waits until `pid` is gone. A `serve` that `attach` started is reaped by launchd once `attach`
/// has exited.
pub async fn gone(pid: Pid) {
    let deadline = Instant::now() + PATIENCE;
    while is_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "process {pid:?} is still running"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

/// Stops whichever `serve` runs for a data folder when dropped, so a failing test doesn't leave
/// a daemon behind. The `serve` that `attach` starts is not a child of the test.
pub struct StopServe(pub PathBuf);

impl Drop for StopServe {
    fn drop(&mut self) {
        let Some(pid) = serve_pid(&self.0) else {
            return;
        };
        let _ = rustix::process::kill_process(pid, Signal::TERM);
        let deadline = std::time::Instant::now() + PATIENCE;
        while is_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = rustix::process::kill_process(pid, Signal::KILL);
    }
}

/// A `wispd serve` that the test started itself.
pub struct Serve {
    child: std::process::Child,
}

impl Serve {
    /// Starts `serve` and waits until it has logged that it listens.
    pub async fn start(data_dir: &Path) -> Self {
        let child = {
            let _lock = spawn_lock();
            std::process::Command::new(WISPD)
                .arg("serve")
                .env("WISPD_DATA_DIR", data_dir)
                .env_remove("WISPD_LOG")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn wispd serve")
        };
        let mut serve = Self { child };
        let deadline = Instant::now() + PATIENCE;
        while !log(data_dir).contains("listening") {
            if let Some(status) = serve.child.try_wait().expect("check on wispd") {
                panic!("wispd serve exited while starting: {status}");
            }
            assert!(Instant::now() < deadline, "wispd serve did not start");
            sleep(Duration::from_millis(10)).await;
        }
        serve
    }

    pub fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id().try_into().unwrap()).unwrap()
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().expect("check on wispd").is_none()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.pid(), Signal::TERM);
        let deadline = std::time::Instant::now() + PATIENCE;
        while std::time::Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A running `wispd attach`, with its stdio piped to the test.
pub struct Attach {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: FramedRead<ChildStdout, FrameCodec>,
    stderr: ChildStderr,
    next_id: i64,
}

/// How an `attach` ended, and everything it wrote after the messages the test read.
pub struct Exited {
    pub status: ExitStatus,
    pub stdout: Vec<Message>,
    pub stderr: String,
}

impl fmt::Debug for Exited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}, {} more messages, stderr: {:?}",
            self.status,
            self.stdout.len(),
            self.stderr
        )
    }
}

impl Attach {
    /// Starts `wispd attach` for `data_dir`.
    pub fn spawn(data_dir: &Path) -> Self {
        Self::spawn_with(data_dir, &[], None)
    }

    /// Starts `wispd attach` for `data_dir` with more arguments, and optionally with one more
    /// descriptor that it inherits without close-on-exec, as a careless parent might pass it.
    pub fn spawn_with(data_dir: &Path, args: &[&str], inherit: Option<BorrowedFd<'_>>) -> Self {
        let mut command = Command::new(WISPD);
        command
            .arg("attach")
            .args(args)
            .env("WISPD_DATA_DIR", data_dir)
            .env_remove("WISPD_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = {
            let _lock = spawn_lock();
            let leaked = inherit.map(|fd| rustix::io::dup(fd).expect("copy the descriptor"));
            let child = command.spawn().expect("spawn wispd attach");
            drop(leaked);
            child
        };
        Self {
            stdin: child.stdin.take(),
            stdout: FramedRead::new(child.stdout.take().unwrap(), FrameCodec::new()),
            stderr: child.stderr.take().unwrap(),
            child,
            next_id: 0,
        }
    }

    pub fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id().unwrap().try_into().unwrap()).unwrap()
    }

    pub fn signal(&self, signal: Signal) {
        rustix::process::kill_process(self.pid(), signal).expect("signal wispd attach");
    }

    /// Writes one message as a line to `attach`'s stdin.
    pub async fn send(&mut self, message: &impl serde::Serialize) {
        let mut line = serde_json::to_vec(message).unwrap();
        line.push(b'\n');
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(&line).await.expect("write to attach");
        stdin.flush().await.expect("flush to attach");
    }

    /// Sends a request and returns its id without waiting for the answer.
    pub async fn request<M: RequestMethod>(&mut self, params: M::Params) -> RequestId {
        self.next_id += 1;
        let id = RequestId::Number(self.next_id);
        self.send(&Request::new::<M>(id.clone(), params)).await;
        id
    }

    /// Sends a request and waits for its answer, which must be the next message.
    pub async fn call<M: RequestMethod>(
        &mut self,
        params: M::Params,
    ) -> Result<M::Result, ErrorObject> {
        let id = self.request::<M>(params).await;
        let response = self.response().await;
        assert_eq!(response.id, Some(id), "answers come back in order here");
        response.into_result()
    }

    pub async fn initialize(&mut self) -> InitializeResult {
        self.call::<Initialize>(initialize_params())
            .await
            .expect("initialize")
    }

    /// The next message on stdout. Every line there must be a protocol message.
    pub async fn next(&mut self) -> Option<Message> {
        let frame = timeout(PATIENCE, self.stdout.next())
            .await
            .expect("a message or the end of stdout")?
            .expect("read stdout");
        Some(Message::from_frame(&frame).expect("only protocol messages on stdout"))
    }

    pub async fn response(&mut self) -> Response {
        match self.next().await {
            Some(Message::Response(response)) => response,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    /// Ends `attach`'s input, as the editor does when it is done, or `printf` at its end.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for `attach` to exit, then reads the rest of its stdout and stderr.
    ///
    /// Both must reach their end right after the exit. A pipe that stays open means another
    /// process holds it, such as a `serve` that inherited `attach`'s stdio.
    pub async fn exit(mut self) -> Exited {
        let status = timeout(PATIENCE, self.child.wait())
            .await
            .expect("attach exits")
            .expect("wait for attach");
        let mut stdout = Vec::new();
        while let Some(frame) = timeout(PATIENCE, self.stdout.next())
            .await
            .expect("stdout stayed open after attach exited")
        {
            stdout.push(
                Message::from_frame(&frame.expect("read stdout"))
                    .expect("only protocol messages on stdout"),
            );
        }
        let mut stderr = String::new();
        timeout(PATIENCE, self.stderr.read_to_string(&mut stderr))
            .await
            .expect("stderr stayed open after attach exited")
            .expect("read stderr");
        Exited {
            status,
            stdout,
            stderr,
        }
    }
}

pub fn initialize_params() -> InitializeParams {
    InitializeParams {
        protocol: ProtocolRange::SUPPORTED,
        client: ClientInfo {
            name: "wispd-attach-tests".to_owned(),
            version: "0.0.0".to_owned(),
            machine_id: None,
        },
        capabilities: Capabilities::default(),
    }
}

pub fn create_params(name: &str) -> ProjectCreateParams {
    ProjectCreateParams {
        id: ProjectId::generate(),
        name: name.to_owned(),
        repo_path: format!("/Users/me/src/{name}"),
    }
}

/// The paths of `pid`'s descriptors 0 to 2, from `lsof`.
pub fn stdio_paths(pid: Pid) -> Vec<String> {
    lsof_names(pid, "0-2")
}

/// `pid`'s working directory, from `lsof`.
pub fn working_dir(pid: Pid) -> Vec<String> {
    lsof_names(pid, "cwd")
}

fn lsof_names(pid: Pid, descriptors: &str) -> Vec<String> {
    let lsof = {
        let _lock = spawn_lock();
        std::process::Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &pid.as_raw_nonzero().to_string()])
            .args(["-d", descriptors, "-F", "n"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("run lsof")
    };
    let output = lsof.wait_with_output().expect("run lsof");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(str::to_owned)
        .collect()
}

/// A pipe, made under [`spawn_lock`] so that no process another test spawns inherits it.
pub fn pipe() -> (PipeReader, PipeWriter) {
    let _lock = spawn_lock();
    std::io::pipe().expect("make a pipe")
}

/// Reads `file` to its end on another thread, and says whether the end came within `within`.
pub async fn ends_within(mut file: impl Read + Send + 'static, within: Duration) -> bool {
    let reading = tokio::task::spawn_blocking(move || {
        let mut rest = Vec::new();
        file.read_to_end(&mut rest).map(|_| rest)
    });
    matches!(timeout(within, reading).await, Ok(Ok(Ok(_))))
}

/// Holds the instance lock on `data_dir`, as a running `serve` does, while it lives.
pub fn hold_instance_lock(data_dir: &Path) -> File {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("wispd.lock"))
        .expect("open the lock file");
    file.try_lock().expect("take the lock");
    file
}
