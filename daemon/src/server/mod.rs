//! The server behind `wispd serve` (decision record 0007).
//!
//! [`Server::start`] runs the startup checks in order: the data folder, the instance lock, the old
//! socket, the new socket, and the store. [`Server::run`] then accepts connections until
//! [`Shutdown::trigger`] is called, and shuts down gracefully.

mod connection;
mod setup;

use std::io;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::{UnixListener, UnixStream};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, info, info_span, warn};

pub use setup::prepare_data_dir;
use setup::{InstanceLock, Socket};

use crate::VERSION;
use crate::context::ContextIndex;
use crate::event_log::EventLog;
use crate::keystore::{KeyStore, KeychainStore};
use crate::methods;
use crate::paths::DataDir;
use crate::store::StoreHandle;

const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// `serve` exits with this when another `wispd serve` already runs for the data folder (0009).
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
    /// not. 60 s by default.
    pub socket_check_interval: Duration,
    /// How long a shutdown waits for in-flight requests before it cancels them. 10 s by default.
    pub shutdown_grace: Duration,
    /// How many of the newest events the log keeps for replay. 10,000 by default.
    pub event_retention: usize,
    /// Requests one connection may have in flight before the server stops reading from it.
    /// 32 by default.
    pub max_requests_in_flight: usize,
    /// Replies one connection may have waiting to be written. 32 by default.
    pub outbound_queue: usize,
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
            max_requests_in_flight: 32,
            outbound_queue: 32,
        }
    }
}

/// Why a server could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// Another `wispd serve` holds the lock on this data folder.
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
    /// Something that is not a socket is where the socket goes, so wispd won't remove it.
    #[error("{} is not a socket, so wispd won't remove it", .path.display())]
    NotASocket {
        /// The socket path.
        path: PathBuf,
    },
    /// A file operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What wispd was doing.
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
    /// Where key accounts' API keys live (#117): the real login Keychain, except in tests.
    pub keys: Arc<dyn KeyStore>,
    /// wispd's data folder, so `context/*` (#155) and #156's backends can find a project's shared
    /// context folder.
    pub data_dir: DataDir,
    /// In-memory bookkeeping for shared context writes (#155): idempotency and `lastWriter`.
    pub context: ContextIndex,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub idle_timeout: Duration,
    pub max_requests_in_flight: usize,
    pub outbound_queue: usize,
}

/// A started server, bound to its socket and holding the instance lock.
pub struct Server {
    config: Config,
    daemon: Arc<Daemon>,
    lock: InstanceLock,
    socket: Socket,
    listener: StdUnixListener,
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
            .field("listener", &self.listener)
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
    /// Prepares the data folder, takes the instance lock, removes an old socket, binds the new
    /// one, and opens the store.
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
        let socket_path = data_dir
            .socket_path()
            .map_err(|error| StartError::io("finding the socket path", error))?;
        if socket_path.fallback {
            info!(
                path = %socket_path.path.display(),
                "the socket path in the data folder is too long, so the socket is in the user's temporary folder"
            );
        }
        let (socket, listener) = Socket::bind(&socket_path.path)?;
        let daemon = Arc::new(Daemon {
            started: Instant::now(),
            log: Arc::new(EventLog::new(config.event_retention)),
            store: StoreHandle::open(&data_dir.store_file()),
            os: methods::os_version(),
            limits: Limits {
                idle_timeout: config.idle_timeout,
                max_requests_in_flight: config.max_requests_in_flight.max(1),
                outbound_queue: config.outbound_queue.max(1),
            },
            keys: Arc::new(KeychainStore::new()),
            data_dir: data_dir.clone(),
            context: ContextIndex::default(),
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
            version = VERSION,
            pid = std::process::id(),
            data_dir = %data_dir.root().display(),
            socket = %socket.path().display(),
            log_id = %daemon.log.id(),
            "listening"
        );
        Ok(Self {
            config,
            daemon,
            lock,
            socket,
            listener,
            context_watcher,
        })
    }

    /// The socket the server listens on.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.socket.path()
    }

    /// Accepts connections until `shutdown` is triggered, then shuts down gracefully.
    ///
    /// Shutting down stops accepting connections and removes the socket, stops reading requests,
    /// waits for the ones in flight, stops the store, and removes the lock file.
    ///
    /// # Errors
    ///
    /// If the socket can't be registered with the runtime.
    pub async fn run(self, shutdown: Shutdown) -> io::Result<()> {
        let Self {
            config,
            daemon,
            lock,
            mut socket,
            listener,
            context_watcher,
        } = self;
        // Kept alive to the end of `run`, so the watch lasts exactly as long as the server does.
        let _context_watcher = context_watcher;
        let mut listener = match UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                socket.remove();
                daemon.store.stop().await;
                lock.release();
                return Err(error);
            }
        };
        let connections = TaskTracker::new();
        let abort = CancellationToken::new();
        let euid = rustix::process::geteuid().as_raw();
        let period = config.socket_check_interval;
        let mut check = time::interval_at(time::Instant::now() + period, period);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut connection_id = 0_u64;

        loop {
            tokio::select! {
                biased;
                () = shutdown.graceful.cancelled() => break,
                _ = check.tick() => rebind(&mut socket, &mut listener),
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        connection_id += 1;
                        let accepted = Accepted {
                            daemon: &daemon,
                            connections: &connections,
                            stop_reading: &shutdown.graceful,
                            abort: &abort,
                        };
                        accepted.spawn(stream, connection_id, euid);
                    }
                    Err(error) => {
                        warn!(%error, "could not accept a connection");
                        time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }

        drop(listener);
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

impl Accepted<'_> {
    // Only this user's processes may connect. The folder's permissions already keep others
    // out; this checks again with getpeereid.
    fn spawn(&self, stream: UnixStream, id: u64, euid: u32) {
        let span = info_span!("connection", id);
        match stream.peer_cred() {
            Ok(peer) if peer.uid() == euid => {
                let serve = connection::serve(
                    stream,
                    Arc::clone(self.daemon),
                    self.stop_reading.clone(),
                    self.abort.child_token(),
                );
                self.connections.spawn(serve.instrument(span));
            }
            Ok(peer) => {
                warn!(parent: &span, uid = peer.uid(), "refused a connection from another user");
            }
            Err(error) => {
                warn!(parent: &span, %error, "refused a connection whose user can't be read");
            }
        }
    }
}

fn rebind(socket: &mut Socket, listener: &mut UnixListener) {
    match socket.rebind_if_gone() {
        Ok(None) => {}
        Ok(Some(bound)) => match UnixListener::from_std(bound) {
            Ok(bound) => {
                *listener = bound;
                info!(path = %socket.path().display(), "the socket was gone, so wispd bound it again");
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
        Arc::new(Self {
            started: Instant::now(),
            log: Arc::new(EventLog::new(event_retention)),
            store: StoreHandle::open(&dir.join("wispd.sqlite3")),
            os: "test".to_owned(),
            limits: Limits {
                idle_timeout,
                max_requests_in_flight: 32,
                outbound_queue: 32,
            },
            keys: Arc::new(crate::keystore::MemoryKeyStore::new()),
            data_dir: DataDir::new(dir).unwrap(),
            context: ContextIndex::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    use super::{Accepted, Daemon};

    // A socketpair's peer is this process, so expecting another uid stands in for a client
    // that runs as another user.
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
        let euid = rustix::process::geteuid().as_raw();

        let (server, mut client) = UnixStream::pair().unwrap();
        accepted.spawn(server, 1, euid.wrapping_add(1));
        assert!(connections.is_empty());
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0, "closed at once");

        let (server, _client) = UnixStream::pair().unwrap();
        accepted.spawn(server, 2, euid);
        assert_eq!(connections.len(), 1);
        token.cancel();
        connections.close();
        connections.wait().await;
    }
}
