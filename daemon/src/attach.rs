//! `wispd attach`: how the editor reaches wispd, on this Mac or over SSH (0007, 0010).
//!
//! [`connect`] reaches wispd's socket, and starts wispd first if nothing accepts connections
//! there. [`bridge`] then copies stdin to the socket and the socket to stdout, byte for byte,
//! with no framing of its own. Over SSH, stdout is the protocol stream, so `attach` writes
//! nothing else there. Its own messages go to stderr, through [`report`].

use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, WaitOptions};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::launch_agent::LaunchAgent;
use crate::logging;
use crate::paths::DataDir;
use crate::server::{self, EXIT_ALREADY_RUNNING};
use crate::spawn::{self, Stdio};

/// `attach` exits with this when it never reached wispd: it couldn't start wispd, the `serve` it
/// started stopped, or nothing accepted a connection before the timeout.
pub const EXIT_UNAVAILABLE: u8 = 4;

/// How long `attach` waits for wispd by default: the editor's liveness window (0007).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The first retry comes soon, since a `serve` that is starting binds its socket in milliseconds.
const FIRST_RETRY: Duration = Duration::from_millis(10);
/// Retries double up to this, which also paces starting `serve` again while another holds the
/// lock.
const MAX_RETRY: Duration = Duration::from_millis(500);
/// An error quotes the last line from at most this much of the end of the log.
const QUOTED_LOG_BYTES: u64 = 64 * 1024;
const QUOTED_LINE_CHARS: usize = 300;

/// How [`connect`] reaches wispd.
#[derive(Clone, Debug)]
pub struct Options {
    /// The `wispd` executable that runs `serve`: the running one, except in tests.
    pub program: PathBuf,
    /// How long to wait for wispd to accept a connection, including the time to start it.
    pub connect_timeout: Duration,
    /// The launch agent that starts wispd, if one is installed for the data folder.
    pub launch_agent: Option<LaunchAgent>,
}

/// Why [`connect`] could not reach wispd.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Unavailable {
    /// The socket's path couldn't be worked out.
    #[error("could not find wispd's socket: {0}")]
    SocketPath(#[source] io::Error),
    /// Connecting failed in a way that starting wispd can't fix, such as a permission error.
    #[error("could not connect to {}: {source}", .path.display())]
    Connect {
        /// The socket.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// `serve` couldn't be started.
    #[error("could not start wispd: {0}")]
    Start(String),
    /// The `serve` that `attach` started stopped before it accepted a connection.
    #[error(
        "wispd serve stopped before it accepted a connection ({status}){}. Its log is {}",
        .said.as_ref().map_or_else(String::new, |said| format!(": {said}")),
        .log.display()
    )]
    Exited {
        /// How it stopped.
        status: ExitStatus,
        /// The last line it wrote to its log, if any.
        said: Option<String>,
        /// The log.
        log: PathBuf,
    },
    /// Nothing accepted a connection before the timeout.
    #[error(
        "nothing accepted a connection at {} within {timeout:?}{}. wispd's log is {}",
        .socket.display(),
        .launch_agent.as_ref().map_or_else(String::new, |service| format!(" after `launchctl kickstart {service}`")),
        .log.display()
    )]
    TimedOut {
        /// The socket.
        socket: PathBuf,
        /// How long `attach` waited.
        timeout: Duration,
        /// The launch agent service that `attach` started, if it started wispd that way.
        launch_agent: Option<String>,
        /// The log.
        log: PathBuf,
    },
}

/// Writes `wispd attach: <message>` to stderr.
///
/// Unlike `eprintln!`, it doesn't panic when stderr is closed, as it is once an SSH connection
/// has dropped.
pub fn report(message: impl fmt::Display) {
    let _ = writeln!(io::stderr(), "wispd attach: {message}");
}

/// Connects to wispd's socket for `data_dir`, and starts wispd if nothing accepts connections
/// there.
///
/// wispd is started once. That goes through the launch agent when [`Options::launch_agent`] names
/// one. Otherwise, or if the launch agent can't be started, it spawns `serve` detached: in a new
/// session, with stdin on `/dev/null`, stdout and stderr appended to its log, and no other
/// descriptors. It then retries with backoff until [`Options::connect_timeout`] has passed. A
/// `serve` that exits 3, because another one holds the lock, is started again at the next retry
/// (0009). One that stops any other way ends the wait.
///
/// It never stops a wispd, including one it started.
///
/// # Errors
///
/// [`Unavailable`] when no connection was made.
pub fn connect(data_dir: &DataDir, options: &Options) -> Result<StdUnixStream, Unavailable> {
    let socket = data_dir
        .socket_path()
        .map_err(Unavailable::SocketPath)?
        .path;
    if let Some(stream) = try_connect(&socket)? {
        return Ok(stream);
    }
    let deadline = Instant::now() + options.connect_timeout;
    let mut starter = Starter {
        data_dir,
        options,
        launched: None,
        spawned: None,
    };
    starter.start()?;
    let mut retry = FIRST_RETRY;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Unavailable::TimedOut {
                socket,
                timeout: options.connect_timeout,
                launch_agent: starter.launched,
                log: data_dir.log_file(),
            });
        }
        thread::sleep(retry.min(deadline - now));
        retry = (retry * 2).min(MAX_RETRY);
        if let Some(stream) = try_connect(&socket)? {
            return Ok(stream);
        }
        starter.check()?;
    }
}

/// A connection, or `None` when nothing accepts connections at `path` yet: no socket file, or
/// one that nothing listens on.
fn try_connect(path: &Path) -> Result<Option<StdUnixStream>, Unavailable> {
    match StdUnixStream::connect(path) {
        Ok(stream) => Ok(Some(stream)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(Unavailable::Connect {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Starts wispd, and watches the `serve` it spawned until a connection works.
struct Starter<'a> {
    data_dir: &'a DataDir,
    options: &'a Options,
    /// The launch agent service that was started, if wispd was started that way.
    launched: Option<String>,
    /// The `serve` spawned last, until it exits.
    spawned: Option<Spawned>,
}

struct Spawned {
    pid: Pid,
    /// The log's length when it started, so an error can quote what it wrote.
    log_len: u64,
}

impl Starter<'_> {
    fn start(&mut self) -> Result<(), Unavailable> {
        if let Some(agent) = &self.options.launch_agent {
            match agent.kickstart() {
                Ok(()) => {
                    self.launched = Some(agent.service.clone());
                    return Ok(());
                }
                Err(error) => report(format_args!("{error}; starting wispd serve instead")),
            }
        }
        self.spawn()
    }

    // The data folder is checked first, as `serve` would check it, so the log is never created
    // through a symlink or in a folder that belongs to someone else.
    fn spawn(&mut self) -> Result<(), Unavailable> {
        server::prepare_data_dir(self.data_dir.root())
            .map_err(|error| Unavailable::Start(error.to_string()))?;
        let log_path = self.data_dir.log_file();
        let log = logging::open_log_file(&log_path).map_err(|error| {
            Unavailable::Start(format!("could not open {}: {error}", log_path.display()))
        })?;
        let log_len = log.metadata().map_or(0, |metadata| metadata.len());
        let null = File::open("/dev/null")
            .map_err(|error| Unavailable::Start(format!("could not open /dev/null: {error}")))?;
        let mut command = self.data_dir.command(&self.options.program);
        command.arg("serve");
        let stdio = Stdio {
            stdin: null.as_fd(),
            stdout: log.as_fd(),
            stderr: log.as_fd(),
        };
        let pid = spawn::spawn_detached(&command, stdio).map_err(|error| {
            Unavailable::Start(format!(
                "could not run {} serve: {error}",
                self.options.program.display()
            ))
        })?;
        self.spawned = Some(Spawned { pid, log_len });
        Ok(())
    }

    /// Checks on the `serve` spawned last. One that another `serve` kept out with the lock is
    /// started again.
    fn check(&mut self) -> Result<(), Unavailable> {
        let Some(spawned) = &self.spawned else {
            return Ok(());
        };
        let status = match rustix::process::waitpid(Some(spawned.pid), WaitOptions::NOHANG) {
            Ok(None) => return Ok(()),
            Ok(Some((_, status))) => ExitStatus::from_raw(status.as_raw()),
            Err(error) => {
                return Err(Unavailable::Start(format!(
                    "could not check on wispd serve: {error}"
                )));
            }
        };
        let log_len = spawned.log_len;
        self.spawned = None;
        if status.code() == Some(i32::from(EXIT_ALREADY_RUNNING)) {
            return self.spawn();
        }
        let log = self.data_dir.log_file();
        Err(Unavailable::Exited {
            status,
            said: last_line(&log, log_len),
            log,
        })
    }
}

/// The last line written to the log at `path` since it was `start` bytes long, cut short.
fn last_line(path: &Path, start: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(
        start.max(len.saturating_sub(QUOTED_LOG_BYTES)),
    ))
    .ok()?;
    let mut written = Vec::new();
    file.read_to_end(&mut written).ok()?;
    let written = String::from_utf8_lossy(&written);
    let line = written
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?;
    Some(line.chars().take(QUOTED_LINE_CHARS).collect())
}

/// Copies `input` to wispd and wispd's bytes to `output`, unchanged, until wispd closes the
/// connection or `output` closes.
///
/// When `input` ends, it shuts down the socket's write side and keeps copying, so wispd answers
/// everything it was sent before it closes (0007). A peer that has gone away ends the bridge
/// without an error.
///
/// # Errors
///
/// Any other I/O error.
pub async fn bridge<I, O>(mut input: I, mut output: O, socket: UnixStream) -> io::Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (mut from_wispd, mut to_wispd) = socket.into_split();
    let upstream = async {
        let copied = tokio::io::copy(&mut input, &mut to_wispd).await;
        let shut_down = to_wispd.shutdown().await;
        copied.and(shut_down)
    };
    let downstream = tokio::io::copy(&mut from_wispd, &mut output);
    tokio::pin!(upstream, downstream);
    let mut sending = true;
    loop {
        tokio::select! {
            copied = &mut downstream => return gone_is_fine(copied.map(drop)),
            copied = &mut upstream, if sending => {
                sending = false;
                gone_is_fine(copied)?;
            }
        }
    }
}

fn gone_is_fine(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::net::unix::pipe;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    use super::{bridge, last_line};

    const PATIENCE: Duration = Duration::from_secs(10);

    /// A bridge between two pipes, standing in for stdin and stdout, and a socket whose other
    /// end stands in for wispd.
    struct Rig {
        stdin: pipe::Sender,
        stdout: pipe::Receiver,
        wispd: UnixStream,
        bridge: JoinHandle<io::Result<()>>,
    }

    fn rig() -> Rig {
        let (stdin, input) = pipe::pipe().unwrap();
        let (output, stdout) = pipe::pipe().unwrap();
        let (socket, wispd) = UnixStream::pair().unwrap();
        Rig {
            stdin,
            stdout,
            wispd,
            bridge: tokio::spawn(bridge(input, output, socket)),
        }
    }

    async fn ended(bridge: JoinHandle<io::Result<()>>) -> io::Result<()> {
        timeout(PATIENCE, bridge)
            .await
            .expect("the bridge ends")
            .expect("the bridge task")
    }

    /// Every byte value, NUL, CR, LF, and invalid UTF-8 included, and no final newline.
    fn bytes(len: usize, step: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from(i * step % 251).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn bytes_pass_both_ways_at_once_unchanged() {
        let Rig {
            mut stdin,
            mut stdout,
            wispd,
            bridge,
        } = rig();
        // Larger than every buffer on the way, so both directions must flow at the same time.
        let up = bytes(4 << 20, 7);
        let down = bytes(4 << 20, 13);

        let editor_writes = async {
            stdin.write_all(&up).await.unwrap();
            drop(stdin);
        };
        let editor_reads = async {
            let mut got = Vec::new();
            stdout.read_to_end(&mut got).await.unwrap();
            got
        };
        let wispd_side = async {
            let (mut read, mut write) = wispd.into_split();
            let reads = async {
                let mut got = Vec::new();
                read.read_to_end(&mut got).await.unwrap();
                got
            };
            let writes = async {
                write.write_all(&down).await.unwrap();
            };
            let (got, ()) = tokio::join!(reads, writes);
            // Closing only after the end of the input arrived, as wispd does.
            drop(write);
            got
        };
        let ((), at_editor, at_wispd) = timeout(PATIENCE, async {
            tokio::join!(editor_writes, editor_reads, wispd_side)
        })
        .await
        .expect("the copies finish");

        assert!(at_wispd == up, "the bytes to wispd changed");
        assert!(at_editor == down, "the bytes from wispd changed");
        ended(bridge).await.unwrap();
    }

    #[tokio::test]
    async fn the_end_of_stdin_half_closes_and_the_answer_still_arrives() {
        let Rig {
            mut stdin,
            mut stdout,
            mut wispd,
            bridge,
        } = rig();
        stdin.write_all(b"{\"id\":1}\n").await.unwrap();
        drop(stdin);

        let mut request = Vec::new();
        timeout(PATIENCE, wispd.read_to_end(&mut request))
            .await
            .expect("wispd reads to the end of the input")
            .unwrap();
        assert_eq!(request, b"{\"id\":1}\n");

        wispd
            .write_all(b"{\"id\":1,\"result\":{}}\n")
            .await
            .unwrap();
        drop(wispd);
        let mut answer = Vec::new();
        timeout(PATIENCE, stdout.read_to_end(&mut answer))
            .await
            .expect("the answer and the end of stdout")
            .unwrap();
        assert_eq!(answer, b"{\"id\":1,\"result\":{}}\n");
        ended(bridge).await.unwrap();
    }

    #[tokio::test]
    async fn wispd_closing_ends_the_bridge_while_stdin_is_still_open() {
        let Rig {
            stdin,
            mut stdout,
            wispd,
            bridge,
        } = rig();
        drop(wispd);
        ended(bridge).await.unwrap();
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        drop(stdin);
    }

    #[tokio::test]
    async fn a_closed_stdout_ends_the_bridge() {
        let Rig {
            stdin,
            stdout,
            mut wispd,
            bridge,
        } = rig();
        drop(stdout);
        wispd
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"events/event\"}\n")
            .await
            .unwrap();
        ended(bridge).await.unwrap();
        drop(stdin);
    }

    #[test]
    fn an_error_quotes_the_last_line_the_new_serve_logged() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("wispd.log");
        fs::write(&log, "an older line\n").unwrap();
        let start = fs::metadata(&log).unwrap().len();
        assert_eq!(last_line(&log, start), None);

        fs::write(
            &log,
            "an older line\nERROR could not start\nwispd: it failed\n\n",
        )
        .unwrap();
        assert_eq!(last_line(&log, start).as_deref(), Some("wispd: it failed"));
        fs::write(&log, format!("an older line\n{}\n", "x".repeat(1000))).unwrap();
        assert_eq!(last_line(&log, start).map(|line| line.len()), Some(300));
    }
}
