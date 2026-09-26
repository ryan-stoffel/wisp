//! Logging with `tracing`, to `logs/wispd.log` in the data folder.

use std::fmt;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, IsTerminal};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt as format;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wisp_protocol::jsonrpc::RequestId;

/// The environment variable that sets the log level when `--log-level` is not given.
pub const LOG_LEVEL_ENV: &str = "WISPD_LOG";

/// The log level when neither `--log-level` nor [`LOG_LEVEL_ENV`] is set.
pub const DEFAULT_LOG_LEVEL: &str = "info";

const MAX_LOGGED_CHARS: usize = 64;

/// Text from a client, ready for a log field with `?`.
///
/// It is quoted and escaped, so a newline in it can't start a forged log line. Past 64
/// characters it is cut, with its full length in bytes after it, so a huge value can't make a
/// huge line.
pub(crate) struct Untrusted<'a>(pub &'a str);

impl fmt::Debug for Untrusted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.char_indices().nth(MAX_LOGGED_CHARS) {
            None => write!(f, "{:?}", self.0),
            Some((end, _)) => write!(f, "{:?}... ({} bytes)", &self.0[..end], self.0.len()),
        }
    }
}

/// A request id from a client, ready for a log field with `?`, cut like [`Untrusted`] text.
pub(crate) struct UntrustedId<'a>(pub &'a RequestId);

impl fmt::Debug for UntrustedId<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RequestId::Number(id) => write!(f, "{id}"),
            RequestId::String(id) => Untrusted(id).fmt(f),
        }
    }
}

/// Which log lines to keep.
///
/// It is a level (`off`, `error`, `warn`, `info`, `debug`, or `trace`), or a comma-separated list
/// of `target=level` directives with an optional bare level for everything else, such as
/// `wispd=debug,warn`. Unlike `tracing-subscriber`'s own parser, a word that is not a level is an
/// error rather than a target name, so a typo can't silently turn logging off.
#[derive(Clone, Debug)]
pub struct LogFilter {
    spec: String,
    targets: Targets,
}

impl FromStr for LogFilter {
    type Err = String;

    fn from_str(spec: &str) -> Result<Self, String> {
        let mut targets = Targets::new();
        for directive in spec.split(',').map(str::trim) {
            targets = match directive.split_once('=') {
                Some((target, level)) if !target.is_empty() => {
                    targets.with_target(target, parse_level(level)?)
                }
                Some(_) => return Err(format!("{directive:?} has no target before `=`")),
                None => targets.with_default(parse_level(directive)?),
            };
        }
        Ok(Self {
            spec: spec.trim().to_owned(),
            targets,
        })
    }
}

impl fmt::Display for LogFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spec)
    }
}

fn parse_level(level: &str) -> Result<LevelFilter, String> {
    match level.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(LevelFilter::OFF),
        "error" => Ok(LevelFilter::ERROR),
        "warn" => Ok(LevelFilter::WARN),
        "info" => Ok(LevelFilter::INFO),
        "debug" => Ok(LevelFilter::DEBUG),
        "trace" => Ok(LevelFilter::TRACE),
        _ => Err(format!(
            "unknown log level {level:?}; expected off, error, warn, info, debug, or trace"
        )),
    }
}

/// Opens the log file at `path` for appending. The file (0600) and its folder (0700) are created
/// if they are missing.
///
/// # Errors
///
/// If the folder or the file can't be created or opened.
pub fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Sends log lines to the file at `path`, appending, and to stderr as well when stderr is a
/// terminal.
///
/// Lines go only to the file otherwise, because `wispd attach` and launchd point `serve`'s
/// stderr at that same file to catch panics, and the lines would appear twice.
///
/// # Errors
///
/// If the file can't be opened, or logging was already started.
pub fn init(path: &Path, filter: &LogFilter) -> io::Result<()> {
    let file = open_log_file(path)?;
    let stderr = io::stderr()
        .is_terminal()
        .then(|| format::layer().with_writer(io::stderr));
    tracing_subscriber::registry()
        .with(filter.targets.clone())
        .with(format::layer().with_writer(Mutex::new(file)))
        .with(stderr)
        .try_init()
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use tracing::Level;
    use wisp_protocol::jsonrpc::RequestId;

    use super::{LogFilter, Untrusted, UntrustedId};

    fn allows(filter: &str, target: &str, level: Level) -> bool {
        filter
            .parse::<LogFilter>()
            .unwrap()
            .targets
            .would_enable(target, &level)
    }

    #[test]
    fn a_level_applies_to_every_target() {
        assert!(allows("debug", "wispd::server", Level::DEBUG));
        assert!(!allows("debug", "wispd::server", Level::TRACE));
        assert!(allows("INFO", "anything", Level::WARN));
        assert!(!allows("off", "wispd", Level::ERROR));
    }

    #[test]
    fn targets_take_their_own_levels() {
        let filter = "wispd=debug, warn";
        assert!(allows(filter, "wispd::server", Level::DEBUG));
        assert!(!allows(filter, "rusqlite", Level::INFO));
        assert!(allows(filter, "rusqlite", Level::WARN));
        assert_eq!(
            filter.parse::<LogFilter>().unwrap().to_string(),
            "wispd=debug, warn"
        );
    }

    #[test]
    fn unknown_levels_are_errors() {
        for bad in ["", "verbose", "wispd=loud", "=debug", "info,"] {
            assert!(bad.parse::<LogFilter>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn long_client_text_is_cut_to_64_characters_and_its_length() {
        let huge = "\u{e9}".repeat(1_000_000);
        let logged = format!("{:?}", Untrusted(&huge));
        assert_eq!(
            logged,
            format!("{:?}... (2000000 bytes)", "\u{e9}".repeat(64))
        );
        let exactly = "x".repeat(64);
        assert_eq!(format!("{:?}", Untrusted(&exactly)), format!("{exactly:?}"));
    }

    #[test]
    fn request_ids_are_logged_like_client_text() {
        assert_eq!(format!("{:?}", UntrustedId(&RequestId::Number(-7))), "-7");
        let id = RequestId::String(format!("a\n{}", "b".repeat(100)));
        let logged = format!("{:?}", UntrustedId(&id));
        assert!(logged.starts_with("\"a\\nbbb"), "{logged}");
        assert!(logged.ends_with("... (102 bytes)"), "{logged}");
    }
}
