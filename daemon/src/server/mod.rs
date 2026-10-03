//! The server behind `plxd serve` (decision record 0007).
//!
//! [`Server::start`] runs the startup checks in order: the data folder, the instance lock, the old
//! socket, the new socket, and the store. [`Server::run`] then accepts connections until
//! [`Shutdown::trigger`] is called, and shuts down gracefully.

mod connection;
pub(crate) mod setup;

use std::io;
#[cfg(unix)]
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, info, info_span, warn};

use setup::InstanceLock;
#[cfg(windows)]
use setup::Pipe;
#[cfg(unix)]
use setup::Socket;
pub use setup::prepare_data_dir;

use crate::agents::{self, Agents};
use crate::backend::Backend;
use crate::backend::claude::ClaudeBackend;
use crate::backend::codex::CodexBackend;
use crate::backend::cursor::CursorBackend;
use crate::backend::fake::FakeBackend;
use crate::backend::process::{Environment, Launcher};
use crate::backend::run_temp;
use crate::context::ContextIndex;
use crate::detect::CliDetector;
use crate::event_log::EventLog;
use crate::keystore::{self, KeyStore};
use crate::methods;
use crate::paths::DataDir;
use crate::routing::BackendRegistry;
use crate::store::StoreHandle;
use crate::worktree::WorktreeManager;

const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// `serve` exits with this when another `plxd serve` already runs for the data folder (0009).
pub const EXIT_ALREADY_RUNNING: u8 = 3;

/// How a server runs. [`Config::new`] has the defaults from 0007.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// The data folder.
    pub data_dir: DataDir,
    /// A connection that sends nothing for this long is closed. 90 s by default; the editor
    /// sends `host/health` every 30 s.
    pub idle_timeout: Duration,
    /// How often the server checks that its socket file still exists, and binds it again if
    /// not. 60 s by default. Windows' named pipe needs no check.
    pub socket_check_interval: Duration,
    /// How long a shutdown waits for in-flight requests before it cancels them. 10 s by default.
    pub shutdown_grace: Duration,
    /// How many of the newest events the log keeps in memory for `events/subscribe` replay.
    /// 10,000 by default.
    pub event_retention: usize,
    /// The in-memory replay window's byte bound (#187): even within `event_retention`, evicts
    /// older events once the total size of their JSON (not their in-memory heap size, which is
    /// somewhat larger) exceeds this many bytes. 64 MiB by default, since a run's `agent.output`
    /// batches (up to about 256 KiB each) can otherwise hold far more memory than
    /// `event_retention` alone was sized for. Applied on every append and, defensively, right
    /// after a restart reloads the table too.
    pub event_retention_bytes: usize,
    /// How many of the newest host and project events (not tied to a run, such as
    /// `project.created` and `context.changed`) the stored event log keeps; older ones are
    /// pruned (#187). An agent run's events are never pruned this way: they stay as long as the
    /// run's own row does, and nothing removes a run's row yet. 10,000 by default, the same
    /// figure as `event_retention`.
    ///
    /// Must be at least `event_retention` (`EventLog::with` clamps it if not): a restart only
    /// reloads the newest `event_retention` events, and every host or project event among them is
    /// necessarily among the newest `event_retention` host and project events too, so a smaller
    /// `host_event_retention` could prune one the reload still expects — a gap `resyncRequired`
    /// would never notice (0016).
    pub host_event_retention: usize,
    /// Requests one connection may have in flight before the server stops reading from it.
    /// 32 by default.
    pub max_requests_in_flight: usize,
    /// Replies one connection may have waiting to be written. 32 by default.
    pub outbound_queue: usize,
    /// The backends workers run on (#156). `None`, the default, registers Claude Code for
    /// Anthropic accounts, or the fake backend when `PLXD_FAKE_BACKEND` names a script
    /// ([`FakeBackend::from_env`]), and Codex for `OpenAI` accounts; tests register a fake.
    pub backends: Option<BackendRegistry>,
    /// The environment agent CLIs, CLI probes, and worktree git commands start from. `None`, the
    /// default, is plxd's own with the login shell's `PATH` and the usual install folders filled in
    /// (#96, decision 0014, PLX-323).
    pub agent_environment: Option<Environment>,
    /// How long a run's permission request waits for an answer before plxd denies it (RYA-222,
    /// decision 0031). 30 minutes by default.
    pub approval_timeout: Duration,
    /// The most a run a usage limit stopped resumes past its reset, at random (PLX-371, decision
    /// 0049). 60 seconds by default.
    pub resume_jitter: Duration,
    /// How long such a run first waits when no reset time is known, doubling with each resume
    /// in a row that finds the limit still on, up to 16 times this. 15 minutes by default.
    pub resume_backoff: Duration,
}

impl Config {
    /// The defaults, for the data folder `data_dir`.
    #[must_use]
    pub fn new(data_dir: DataDir) -> Self {
        Self {
            data_dir,
            idle_timeout: Duration::from_secs(90),
            socket_check_interval: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(10),
            event_retention: 10_000,
            event_retention_bytes: 64 * 1024 * 1024,
            host_event_retention: 10_000,
            max_requests_in_flight: 32,
            outbound_queue: 32,
            backends: None,
            agent_environment: None,
            approval_timeout: agents::APPROVAL_TIMEOUT,
            resume_jitter: agents::ResumeTiming::default().jitter,
            resume_backoff: agents::ResumeTiming::default().backoff,
        }
    }
}

/// Why a server could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// Another `plxd serve` holds the lock on this data folder.
    #[error(
        "already running for {}{}",
        .data_dir.display(),
        .pid.map_or_else(String::new, |pid| format!(" (pid {pid})"))
    )]
    AlreadyRunning {
        /// The data folder.
        data_dir: PathBuf,
        /// The running instance's pid, if its lock file says.
        pid: Option<u32>,
    },
    /// The data folder can't be used safely.
    #[error("can't use the data folder {}: {reason}", .path.display())]
    DataDir {
        /// The data folder.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// Something that is not a socket is where the socket goes, so plxd won't remove it. Unix
    /// only.
    #[error("{} is not a socket, so plxd won't remove it", .path.display())]
    NotASocket {
        /// The socket path.
        path: PathBuf,
    },
    /// A file operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What plxd was doing.
        context: String,
        /// The error.
        #[source]
        source: io::Error,
    },
}

impl StartError {
    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Asks a running [`Server`] to stop. Clones share the request.
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    graceful: CancellationToken,
    immediate: CancellationToken,
}

impl Shutdown {
    /// A shutdown that nobody has triggered yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The first call shuts down gracefully: the server stops accepting connections and reading
    /// requests, and lets the requests in flight finish for up to [`Config::shutdown_grace`].
    /// A second call stops waiting for them.
    pub fn trigger(&self) {
        if self.graceful.is_cancelled() {
            self.immediate.cancel();
        } else {
            self.graceful.cancel();
        }
    }
}

/// What every connection shares.
pub(crate) struct Daemon {
    pub started: Instant,
    pub log: Arc<EventLog>,
    pub store: StoreHandle,
    /// The operating system and version, for `host/version`.
    pub os: String,
    pub limits: Limits,
    /// Detects the vendor CLIs for `accounts/list` and `accounts/refresh` (#114), and `gh` for
    /// `github/status` (PLX-336).
    pub cli_detector: CliDetector,
    /// Where key accounts' API keys live (#117): the OS's real store, except in tests.
    pub keys: Arc<dyn KeyStore>,
    /// plxd's data folder, so `context/*` (#155) and the runner (#156) can find a project's
    /// shared context folder.
    pub data_dir: DataDir,
    /// In-memory bookkeeping for shared context writes (#155): idempotency and `lastWriter`.
    pub context: ContextIndex,
    /// Agent runs (#156).
    pub agents: Agents,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub idle_timeout: Duration,
    pub max_requests_in_flight: usize,
    pub outbound_queue: usize,
}

/// A started server, bound to its socket or pipe and holding the instance lock.
pub struct Server {
    config: Config,
    daemon: Arc<Daemon>,
    lock: InstanceLock,
    #[cfg(unix)]
    socket: Socket,
    #[cfg(unix)]
    listener: StdUnixListener,
    /// The pipe, which is its own listener.
    #[cfg(windows)]
    socket: Pipe,
    /// Kept alive for as long as the server runs; dropping it stops the watch (#155).
    context_watcher: Option<notify::RecommendedWatcher>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("config", &self.config)
            .field("daemon", &self.daemon)
            .field("lock", &self.lock)
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("log_id", &self.log.id())
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Prepares the data folder, takes the instance lock, removes what earlier runs left in their
    /// temp folders, removes an old socket, binds the new one, and opens the store. On Windows it
    /// creates the named pipe instead of the socket, so it must be called inside a tokio runtime
    /// there.
    ///
    /// A store that can't be opened doesn't stop the server; `host/health` reports it.
    ///
    /// # Errors
    ///
    /// [`StartError::AlreadyRunning`] when another server holds the lock, and the other
    /// variants when a startup check fails.
    pub fn start(config: Config) -> Result<Self, StartError> {
        let data_dir = &config.data_dir;
        prepare_data_dir(data_dir.root())?;
        let lock = InstanceLock::acquire(&data_dir.lock_file(), data_dir.root())?;
        run_temp::sweep(data_dir);
        let socket_path = data_dir
            .socket_path()
            .map_err(|error| StartError::io("finding the socket path", error))?;
        if socket_path.fallback {
            info!(
                path = %socket_path.path.display(),
                "the socket path in the data folder is too long, so the socket is in the user's temporary folder"
            );
        }
        #[cfg(unix)]
        let (socket, listener) = Socket::bind(&socket_path.path)?;
        #[cfg(windows)]
        let socket = Pipe::create(&socket_path.path)?;
        let environment = config
            .agent_environment
            .clone()
            .unwrap_or_else(agents::worker::agent_environment);
        let launcher = Launcher::new(data_dir.clone(), environment);
        let fake = FakeBackend::from_env(&launcher)
            .map_err(|error| StartError::io("setting up the fake backend", error))?;
        let backends = config.backends.clone().unwrap_or_else(|| {
            let backend: Arc<dyn Backend> = match fake {
                Some(fake) => Arc::new(fake),
                None => Arc::new(ClaudeBackend::new(launcher.clone())),
            };
            let mut backends = BackendRegistry::new();
            backends.register(parallax_protocol::Provider::Anthropic, backend);
            backends.register(
                parallax_protocol::Provider::Openai,
                Arc::new(CodexBackend::new(launcher.clone())),
            );
            backends.register(
                parallax_protocol::Provider::Cursor,
                Arc::new(CursorBackend::new(launcher.clone())),
            );
            backends
        });
        let worktrees = WorktreeManager::new(launcher.clone(), data_dir.root());
        let store = StoreHandle::open(&data_dir.store_file());
        let daemon = Arc::new(Daemon {
            started: Instant::now(),
            log: Arc::new(EventLog::open(
                &data_dir.store_file(),
                config.event_retention,
                config.event_retention_bytes,
                config.host_event_retention,
            )),
            store,
            os: methods::os_version(),
            cli_detector: CliDetector::new(launcher, crate::detect::PROBE_TIMEOUT),
            limits: Limits {
                idle_timeout: config.idle_timeout,
                max_requests_in_flight: config.max_requests_in_flight.max(1),
                outbound_queue: config.outbound_queue.max(1),
            },
            keys: keystore::system_store(),
            data_dir: data_dir.clone(),
            context: ContextIndex::default(),
            agents: Agents::new(backends, worktrees)
                .with_approval_timeout(config.approval_timeout)
                .with_resume_timing(agents::ResumeTiming {
                    jitter: config.resume_jitter,
                    backoff: config.resume_backoff,
                }),
        });
        // Best effort: a project's context folder is also ensured lazily on its first
        // `context/*` call (#155), so a watcher that fails to start only loses live updates for
        // agents' own writes, not the feature.
        if let Err(error) = std::fs::create_dir_all(data_dir.context_root()) {
            warn!(%error, "could not create the shared context folder");
        }
        let context_watcher = match crate::context::watcher::start(Arc::clone(&daemon)) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                warn!(%error, "could not watch the shared context folder for agents' own writes");
                None
            }
        };
        info!(
            version = crate::version(),
            pid = std::process::id(),
            data_dir = %data_dir.root().display(),
            socket = %socket_path.path.display(),
            log_id = %daemon.log.id(),
            "listening"
        );
        Ok(Self {
            config,
            daemon,
            lock,
            socket,
            #[cfg(unix)]
            listener,
            context_watcher,
        })
    }

    /// The socket, or on Windows the pipe, the server listens on.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.socket.path()
    }

    /// Accepts connections until `shutdown` is triggered, then shuts down gracefully.
    ///
    /// Shutting down stops accepting connections and removes the socket, stops reading requests,
    /// waits for the ones in flight, stops the store, and removes the lock file. Windows keeps the
    /// lock file (0023).
    ///
    /// # Errors
    ///
    /// If the socket can't be registered with the runtime.
    pub async fn run(self, shutdown: Shutdown) -> io::Result<()> {
        let Self {
            config,
            daemon,
            lock,
            #[cfg(unix)]
            mut socket,
            #[cfg(windows)]
            socket,
            #[cfg(unix)]
            listener,
            context_watcher,
        } = self;
        // Kept alive to the end of `run`, so the watch lasts exactly as long as the server does.
        let _context_watcher = context_watcher;
        #[cfg(windows)]
        let mut listener = socket;
        #[cfg(unix)]
        let mut listener = match UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                socket.remove();
                daemon.store.stop().await;
                lock.release();
                return Err(error);
            }
        };
        agents::recover(&daemon).await;
        agents::deliver_queued(&daemon).await;
        let connections = TaskTracker::new();
        let abort = CancellationToken::new();
        let period = config.socket_check_interval;
        let mut check = time::interval_at(time::Instant::now() + period, period);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut connection_id = 0_u64;

        loop {
            tokio::select! {
                biased;
                () = shutdown.graceful.cancelled() => break,
                _ = check.tick(), if cfg!(unix) => {
                    #[cfg(unix)]
                    rebind(&mut socket, &mut listener);
                }
                accepted = listener.accept() => match accepted {
                    Ok(stream) => {
                        #[cfg(unix)]
                        let (stream, _) = stream;
                        connection_id += 1;
                        let accepted = Accepted {
                            daemon: &daemon,
                            connections: &connections,
                            stop_reading: &shutdown.graceful,
                            abort: &abort,
                        };
                        let peer = Peer::of(&stream);
                        accepted.spawn(stream, connection_id, peer);
                    }
                    Err(error) => {
                        warn!(%error, "could not accept a connection");
                        time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }

        drop(listener);
        #[cfg(unix)]
        socket.remove();
        info!("shutting down");
        connections.close();
        let finished = tokio::select! {
            () = connections.wait() => true,
            () = time::sleep(config.shutdown_grace) => {
                warn!(grace = ?config.shutdown_grace, "requests are still running; cancelling them");
                false
            }
            () = shutdown.immediate.cancelled() => {
                info!("stopping without waiting for requests");
                false
            }
        };
        if !finished {
            abort.cancel();
            connections.wait().await;
        }
        daemon.agents.shutdown().await;
        daemon.store.stop().await;
        lock.release();
        info!("stopped");
        Ok(())
    }
}

struct Accepted<'a> {
    daemon: &'a Arc<Daemon>,
    connections: &'a TaskTracker,
    stop_reading: &'a CancellationToken,
    abort: &'a CancellationToken,
}

/// Who is at the other end of a connection.
#[derive(Debug)]
enum Peer {
    ThisUser,
    /// Another user, as the OS identifies them or their process.
    Other(String),
    /// Nobody knows: reading the peer failed.
    Unknown(io::Error),
}

impl Peer {
    /// With getpeereid. The folder's permissions already keep other users out; this checks
    /// again.
    #[cfg(unix)]
    fn of(stream: &tokio::net::UnixStream) -> Self {
        let euid = rustix::process::geteuid().as_raw();
        match stream.peer_cred() {
            Ok(peer) if peer.uid() == euid => Self::ThisUser,
            Ok(peer) => Self::Other(format!("uid {}", peer.uid())),
            Err(error) => Self::Unknown(error),
        }
    }

    /// With the client's process's user SID. The pipe's DACL already keeps other users out;
    /// this checks again (0023).
    #[cfg(windows)]
    fn of(pipe: &tokio::net::windows::named_pipe::NamedPipeServer) -> Self {
        let pid = match crate::windows::pipe_client_pid(pipe) {
            Ok(pid) => pid,
            Err(error) => return Self::Unknown(error),
        };
        match crate::windows::runs_as_this_user(pid) {
            Ok(true) => Self::ThisUser,
            Ok(false) => Self::Other(format!("pid {pid}")),
            Err(error) => Self::Unknown(error),
        }
    }
}

impl Accepted<'_> {
    /// Serves `stream` if `peer` is this user; only this user's processes may connect.
    fn spawn<S>(&self, stream: S, id: u64, peer: Peer)
    where
        S: AsyncRead + AsyncWrite + Send + Sync + 'static,
    {
        let span = info_span!("connection", id);
        match peer {
            Peer::ThisUser => {
                let serve = connection::serve(
                    stream,
                    Arc::clone(self.daemon),
                    self.stop_reading.clone(),
                    self.abort.child_token(),
                );
                self.connections.spawn(serve.instrument(span));
            }
            Peer::Other(peer) => {
                warn!(parent: &span, %peer, "refused a connection from another user");
            }
            Peer::Unknown(error) => {
                warn!(parent: &span, %error, "refused a connection whose user can't be read");
            }
        }
    }
}

#[cfg(unix)]
fn rebind(socket: &mut Socket, listener: &mut UnixListener) {
    match socket.rebind_if_gone() {
        Ok(None) => {}
        Ok(Some(bound)) => match UnixListener::from_std(bound) {
            Ok(bound) => {
                *listener = bound;
                info!(path = %socket.path().display(), "the socket was gone, so plxd bound it again");
            }
            Err(error) => warn!(%error, "could not listen on the socket it bound again"),
        },
        Err(error) => {
            warn!(path = %socket.path().display(), %error, "could not check the socket or bind it again");
        }
    }
}

#[cfg(test)]
impl Daemon {
    /// A daemon with its store in `dir`, and the default limits except the idle timeout. Its
    /// `KeyStore` is an in-memory mock, never the real Keychain.
    pub(crate) fn for_tests(
        dir: &Path,
        event_retention: usize,
        idle_timeout: Duration,
    ) -> Arc<Self> {
        // An empty PATH, not `Environment::inherited()`: these tests exercise the server, not
        // detection, and must never resolve or run whatever CLIs happen to be on this machine.
        let launcher = Launcher::new(
            DataDir::new(dir.join("cli-detect")).expect("resolve a data folder for the launcher"),
            Environment::empty(),
        );
        let worktrees = WorktreeManager::new(launcher.clone(), dir);
        let store = StoreHandle::open(&dir.join("plxd.sqlite3"));
        Arc::new(Self {
            started: Instant::now(),
            log: Arc::new(EventLog::open(
                &dir.join("plxd.sqlite3"),
                event_retention,
                usize::MAX,
                usize::MAX,
            )),
            store,
            os: "test".to_owned(),
            cli_detector: CliDetector::new(launcher, crate::detect::PROBE_TIMEOUT),
            limits: Limits {
                idle_timeout,
                max_requests_in_flight: 32,
                outbound_queue: 32,
            },
            keys: Arc::new(crate::keystore::MemoryKeyStore::new()),
            data_dir: DataDir::new(dir).unwrap(),
            context: ContextIndex::default(),
            agents: Agents::new(BackendRegistry::new(), worktrees),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    use super::{Accepted, Daemon, Peer};

    #[tokio::test]
    async fn only_a_peer_running_as_this_user_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 10, Duration::from_secs(90));
        let connections = TaskTracker::new();
        let token = CancellationToken::new();
        let accepted = Accepted {
            daemon: &daemon,
            connections: &connections,
            stop_reading: &token,
            abort: &token,
        };

        let (server, mut client) = tokio::io::duplex(64);
        accepted.spawn(server, 1, Peer::Other("uid 1".to_owned()));
        assert!(connections.is_empty());
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0, "closed at once");

        let (server, mut client) = tokio::io::duplex(64);
        accepted.spawn(server, 2, Peer::Unknown(std::io::Error::other("no peer")));
        assert!(connections.is_empty());
        assert_eq!(client.read(&mut byte).await.unwrap(), 0, "closed at once");

        let (server, _client) = tokio::io::duplex(64);
        accepted.spawn(server, 3, Peer::ThisUser);
        assert_eq!(connections.len(), 1);
        token.cancel();
        connections.close();
        connections.wait().await;
    }
}
