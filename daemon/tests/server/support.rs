//! Starting servers in temporary folders and talking to them through `wisp_protocol`.
//!
//! Every test gets its own data folder under `/tmp`, which keeps socket paths well under macOS's
//! 103-byte limit and keeps tests away from the real data folder.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::codec::Framed;
use wisp_protocol::framing::FrameCodec;
use wisp_protocol::jsonrpc::{ErrorObject, Message, Notification, Request, RequestId, Response};
use wisp_protocol::methods::{
    EventsEvent, EventsSubscribe, Initialize, NotificationMethod, RequestMethod,
};
use wisp_protocol::{
    AgentStatus, Capabilities, ClientInfo, ErrorKind, EventsEventParams, EventsSubscribeParams,
    InitializeParams, InitializeResult, ProjectCreateParams, ProjectId, ProtocolRange, Provider,
    WispEvent,
};
use wispd::backend::Event;
use wispd::backend::fake::{FakeBackend, Script, Step};
use wispd::backend::process::{CancelPolicy, Environment, Launcher};
use wispd::paths::DataDir;
use wispd::routing::BackendRegistry;
use wispd::server::{Config, Server, Shutdown};

/// How long a test waits for anything before it fails.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// A fresh folder under `/tmp`, removed when dropped.
pub fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("wispd-")
        .tempdir_in("/tmp")
        .expect("create a temp dir under /tmp")
}

/// Where the server for `data_dir` listens, by the same rule `attach` will use.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    DataDir::new(data_dir)
        .expect("resolve the data folder")
        .socket_path()
        .expect("find the socket path")
        .path
}

/// A `wispd serve` process.
pub struct Wispd {
    child: Child,
    pub socket: PathBuf,
}

impl Wispd {
    /// Starts `wispd serve --data-dir <data_dir>` and waits until it accepts connections.
    pub async fn start(data_dir: &Path) -> Self {
        Self::start_with(data_dir, &[], &[]).await
    }

    /// Ready means this wispd answered a handshake, not just that a connect worked: a listener
    /// that another test's child inherited can accept connects at the same path and answer
    /// nothing (#86).
    pub async fn start_with(data_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = spawn(data_dir, args, env);
        let socket = socket_path(data_dir);
        let deadline = Instant::now() + PATIENCE;
        loop {
            if answers_handshake(&socket).await {
                return Self { child, socket };
            }
            if let Some(status) = child.try_wait().expect("check on wispd") {
                panic!("wispd exited while starting: {status}");
            }
            assert!(Instant::now() < deadline, "wispd did not start answering");
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn signal(&self, signal: Signal) {
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        kill_process(pid, signal).expect("signal wispd");
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().expect("check on wispd").is_none()
    }

    /// Waits for the process to exit and returns its status and stderr.
    pub async fn exit(mut self) -> (ExitStatus, String) {
        let status = wait(&mut self.child).await;
        (status, stderr(&mut self.child))
    }
}

impl Drop for Wispd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn answers_handshake(socket: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(socket).await else {
        return false;
    };
    let mut framed = Framed::new(stream, FrameCodec::new());
    if framed
        .send(&Request::new::<Initialize>(
            1,
            initialize_params(ProtocolRange::SUPPORTED),
        ))
        .await
        .is_err()
    {
        return false;
    }
    let Ok(Some(Ok(frame))) = timeout(Duration::from_secs(1), framed.next()).await else {
        return false;
    };
    matches!(Message::from_frame(&frame), Ok(Message::Response(response)) if response.result.is_ok())
}

fn initialize_params(protocol: ProtocolRange) -> InitializeParams {
    InitializeParams {
        protocol,
        client: ClientInfo {
            name: "wispd-tests".to_owned(),
            version: "0.0.0".to_owned(),
            machine_id: None,
        },
        capabilities: Capabilities::default(),
    }
}

/// Runs `wispd serve` for `data_dir` to completion, for a start that is expected to fail.
pub async fn run_to_exit(data_dir: &Path, args: &[&str]) -> (ExitStatus, String) {
    let mut child = spawn(data_dir, args, &[]);
    let status = wait(&mut child).await;
    (status, stderr(&mut child))
}

fn spawn(data_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_wispd"))
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env_remove("WISPD_LOG")
        .env_remove("WISPD_DATA_DIR")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wispd")
}

async fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().expect("check on wispd") {
            return status;
        }
        assert!(Instant::now() < deadline, "wispd did not exit");
        sleep(Duration::from_millis(10)).await;
    }
}

fn stderr(child: &mut Child) -> String {
    let mut text = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        io::Read::read_to_string(&mut pipe, &mut text).expect("read stderr");
    }
    text
}

/// A server running inside the test, for settings the command line doesn't expose.
pub struct InProcess {
    pub socket: PathBuf,
    shutdown: Shutdown,
    task: JoinHandle<io::Result<()>>,
}

impl InProcess {
    pub fn config(data_dir: &Path) -> Config {
        Config::new(DataDir::new(data_dir).unwrap())
    }

    pub fn start(config: Config) -> Self {
        let server = Server::start(config).expect("start the server");
        let socket = server.socket_path().to_owned();
        let shutdown = Shutdown::default();
        let task = tokio::spawn(server.run(shutdown.clone()));
        Self {
            socket,
            shutdown,
            task,
        }
    }

    pub async fn stop(self) {
        self.shutdown.trigger();
        timeout(PATIENCE, self.task)
            .await
            .expect("the server stops")
            .expect("the server task")
            .expect("the server runs cleanly");
    }
}

/// A protocol client on one connection.
pub struct Client {
    framed: Framed<UnixStream, FrameCodec>,
    next_id: i64,
}

impl Client {
    pub async fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connect");
        Self {
            framed: Framed::new(stream, FrameCodec::new()),
            next_id: 0,
        }
    }

    /// Connects and completes the handshake.
    pub async fn ready(socket: &Path) -> Self {
        let mut client = Self::connect(socket).await;
        client.initialize().await.expect("initialize");
        client
    }

    pub async fn initialize(&mut self) -> Result<InitializeResult, ErrorObject> {
        self.initialize_with(ProtocolRange::SUPPORTED).await
    }

    pub async fn initialize_with(
        &mut self,
        protocol: ProtocolRange,
    ) -> Result<InitializeResult, ErrorObject> {
        self.call::<Initialize>(initialize_params(protocol)).await
    }

    /// Sends a request and returns its id without waiting for the answer.
    pub async fn send<M: RequestMethod>(&mut self, params: M::Params) -> RequestId {
        self.next_id += 1;
        let id = RequestId::Number(self.next_id);
        self.send_message(&Request::new::<M>(id.clone(), params))
            .await;
        id
    }

    pub async fn notify<N: NotificationMethod>(&mut self, params: N::Params) {
        self.send_message(&Notification::new::<N>(params)).await;
    }

    pub async fn send_message(&mut self, message: &impl serde::Serialize) {
        self.framed.send(message).await.expect("send a message");
    }

    /// Writes bytes as they are, for frames the codec wouldn't produce.
    pub async fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stream = self.framed.get_mut();
        stream.write_all(bytes).await?;
        stream.flush().await
    }

    /// Closes the write side, as `wispd attach` does when its stdin ends.
    pub async fn close_write(&mut self) {
        self.framed
            .get_mut()
            .shutdown()
            .await
            .expect("shut down writes");
    }

    /// The next message, or `None` when the server closed the connection.
    pub async fn next(&mut self) -> Option<Message> {
        let frame = timeout(PATIENCE, self.framed.next())
            .await
            .expect("a message or the end of the connection")?;
        match frame {
            Ok(frame) => Some(Message::from_frame(&frame).expect("a well-formed message")),
            Err(_) => None,
        }
    }

    /// Whether the connection ends within `within` without another message.
    pub async fn closes_within(&mut self, within: Duration) -> bool {
        match timeout(within, self.framed.next()).await {
            Ok(None | Some(Err(_))) => true,
            Ok(Some(Ok(frame))) => panic!(
                "expected the connection to close, got {}",
                String::from_utf8_lossy(&frame)
            ),
            Err(_) => false,
        }
    }

    /// Asserts that nothing arrives within `within`.
    pub async fn stays_quiet(&mut self, within: Duration) {
        if let Ok(next) = timeout(within, self.framed.next()).await {
            panic!("expected silence, got {next:?}");
        }
    }

    /// The next message, which must be an event.
    pub async fn next_event(&mut self) -> EventsEventParams {
        match self.next().await {
            Some(Message::Notification(notification)) => event(notification),
            other => panic!("expected an event, got {other:?}"),
        }
    }

    pub async fn response(&mut self) -> Response {
        match self.next().await {
            Some(Message::Response(response)) => response,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    /// Sends a request and waits for its answer, which must be the next message.
    pub async fn call<M: RequestMethod>(
        &mut self,
        params: M::Params,
    ) -> Result<M::Result, ErrorObject> {
        let id = self.send::<M>(params).await;
        let response = self.response().await;
        assert_eq!(response.id, Some(id), "answers come back in order here");
        response.into_result()
    }
}

/// The kind of a wisp error, or a panic for any other error.
pub fn kind(error: &ErrorObject) -> ErrorKind {
    error
        .wisp_data()
        .unwrap_or_else(|| panic!("expected a wisp error, got {error:?}"))
        .kind
}

/// Params for a new project named `name`, on a repository made for it under `dir`.
pub fn create_params(dir: &Path, name: &str) -> ProjectCreateParams {
    ProjectCreateParams {
        id: ProjectId::generate(),
        name: name.to_owned(),
        repo_path: repo(dir, name),
    }
}

/// Makes `dir/repos/<name>` look like a repository on `main`, as far as wispd checks, and
/// returns its path.
pub fn repo(dir: &Path, name: &str) -> String {
    let path = dir.join("repos").join(name);
    std::fs::create_dir_all(path.join(".git")).expect("make the repository");
    std::fs::write(path.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
    path.to_str().expect("a UTF-8 path").to_owned()
}

/// Holds SQLite's write lock on the store, so the store's next write waits (up to its 5 s busy
/// timeout) and the jobs behind it stay queued.
pub struct WriteLock {
    connection: rusqlite::Connection,
}

impl WriteLock {
    pub fn take(data_dir: &Path) -> Self {
        let connection =
            rusqlite::Connection::open(data_dir.join("wispd.sqlite3")).expect("open the store");
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .expect("take the write lock");
        Self { connection }
    }

    pub fn release(self) {
        self.connection
            .execute_batch("ROLLBACK")
            .expect("release the write lock");
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A repository `dir/repos/<name>` with one commit and its own identity, as a user's checkout
/// would be.
pub fn real_repo(dir: &Path, name: &str) -> PathBuf {
    let repo = dir.join("repos").join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Backends with the fake CLI, running `steps`, for Anthropic accounts.
pub fn fake(steps: Vec<Step>) -> BackendRegistry {
    let scratch = tempfile::tempdir().unwrap();
    let launcher = Launcher::new(
        DataDir::new(scratch.path()).unwrap(),
        Environment::inherited(),
    );
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(
            FakeBackend::new(launcher, Script { steps }).with_cancel_policy(CancelPolicy {
                signal: Signal::INT,
                group: false,
                // The fake's shell can lose a SIGINT that lands while it forks (#188), so
                // `SIGKILL` follows soon.
                grace: Duration::from_millis(500),
            }),
        ),
    );
    backends
}

pub fn init(session_id: &str) -> Step {
    Step::Init {
        session_id: session_id.to_owned(),
        model: None,
    }
}

pub fn text(text: &str) -> Step {
    Step::Emit(Event::Text {
        message_id: None,
        text: text.to_owned(),
    })
}

pub fn end_turn(result: &str) -> Step {
    Step::EndTurn {
        result: Some(result.to_owned()),
    }
}

pub fn updated_to(status: AgentStatus) -> impl FnMut(&EventsEventParams) -> bool {
    move |event| matches!(&event.event, WispEvent::AgentUpdated { state, .. } if state.status == status)
}

/// A client on which events can arrive between a request and its response. They wait in
/// `pending` for [`Conn::until`], so none is lost.
pub struct Conn {
    client: Client,
    pub pending: VecDeque<EventsEventParams>,
}

fn event(notification: Notification) -> EventsEventParams {
    assert_eq!(
        notification.method,
        <EventsEvent as NotificationMethod>::NAME
    );
    serde_json::from_value(notification.params.expect("params")).expect("an event")
}

impl Conn {
    pub async fn connect(socket: &Path) -> Self {
        Self {
            client: Client::connect(socket).await,
            pending: VecDeque::new(),
        }
    }

    pub async fn ready(socket: &Path) -> Self {
        let mut conn = Self::connect(socket).await;
        conn.initialize().await;
        conn
    }

    pub async fn initialize(&mut self) -> InitializeResult {
        self.client.initialize().await.expect("initialize")
    }

    /// Sends a request without waiting for its response, which [`Conn::responses`] collects.
    pub async fn send<M: RequestMethod>(&mut self, params: M::Params) -> RequestId {
        self.client.send::<M>(params).await
    }

    /// The next response, keeping the events that arrive before it.
    async fn next_response(&mut self) -> Response {
        loop {
            match self.client.next().await {
                Some(Message::Response(response)) => return response,
                Some(Message::Notification(notification)) => {
                    self.pending.push_back(event(notification));
                }
                other => panic!("expected a response, got {other:?}"),
            }
        }
    }

    pub async fn call<M: RequestMethod>(
        &mut self,
        params: M::Params,
    ) -> Result<M::Result, ErrorObject> {
        let id = self.send::<M>(params).await;
        let response = self.next_response().await;
        assert_eq!(response.id, Some(id));
        response.into_result()
    }

    /// The responses to `ids`, in that order.
    pub async fn responses(
        &mut self,
        ids: &[RequestId],
    ) -> Vec<Result<serde_json::Value, ErrorObject>> {
        let mut answers: Vec<Option<Result<serde_json::Value, ErrorObject>>> =
            ids.iter().map(|_| None).collect();
        while answers.iter().any(Option::is_none) {
            let response = self.next_response().await;
            let at = ids
                .iter()
                .position(|id| response.id.as_ref() == Some(id))
                .expect("a response to one of the requests");
            answers[at] = Some(response.into_result());
        }
        answers.into_iter().map(Option::unwrap).collect()
    }

    pub async fn subscribe(&mut self, after: u64, project: Option<ProjectId>) {
        self.call::<EventsSubscribe>(EventsSubscribeParams { after, project })
            .await
            .unwrap();
    }

    /// Events until one matches `done`, which is included.
    pub async fn until(
        &mut self,
        mut done: impl FnMut(&EventsEventParams) -> bool,
    ) -> Vec<EventsEventParams> {
        let deadline = Instant::now() + PATIENCE;
        let mut events = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "gave up waiting; got {events:#?}"
            );
            let event = match self.pending.pop_front() {
                Some(event) => event,
                None => self.client.next_event().await,
            };
            let stop = done(&event);
            events.push(event);
            if stop {
                return events;
            }
        }
    }

    pub async fn stays_quiet(&mut self, within: Duration) {
        assert!(self.pending.is_empty(), "{:?}", self.pending);
        self.client.stays_quiet(within).await;
    }
}
