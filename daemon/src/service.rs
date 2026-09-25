//! `wispd service`: installs, removes, and reports on the per-user `LaunchAgent` that keeps
//! `wispd serve` running (issue #61, decision records 0007 and 0009).
//!
//! [`render_plist`] is pure, so it is covered by a golden-file test. [`install`], [`uninstall`],
//! and [`status`] shell out to `launchctl`'s `bootstrap`, `bootout`, and `print`, per Apple's
//! guidance to prefer them over the deprecated `load` and `unload`.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use wisp_protocol::jsonrpc::{Message, Request};
use wisp_protocol::methods::Initialize;
use wisp_protocol::{Capabilities, ClientInfo, InitializeParams, ProtocolRange};

use crate::VERSION;
use crate::paths::{DATA_DIR_ENV, DataDir};

/// wispd's `LaunchAgent` label: the app's bundle id from `editor/product.json`
/// (`io.github.ryan-stoffel.wisp`, #9) plus `.wispd`.
pub const DEFAULT_LABEL: &str = "io.github.ryan-stoffel.wisp.wispd";

/// The environment variable that overrides the label when `--label` is not given. Only tests
/// should set it, so a test run never touches a real install.
pub const SERVICE_LABEL_ENV: &str = "WISPD_SERVICE_LABEL";

const LAUNCHCTL: &str = "/bin/launchctl";

/// How long [`status`] and the conflict check in [`install`] wait for an `initialize` answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why installing, removing, or checking on the `LaunchAgent` failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// A `serve` is already answering this data folder's socket, and it is not the one launchd
    /// would be managing under this label. Bootstrapping anyway would start a second `serve`
    /// that loses the race for `wispd.lock` (0009's exit code 3) and gets restarted into the
    /// same failure by `KeepAlive`, forever fighting the first `serve` for the lock.
    #[error(
        "wispd is already serving {} outside launchd; stop it, then run \
         `wispd service install` again",
        .data_dir.display()
    )]
    AlreadyRunningOutsideLaunchd {
        /// The data folder it is serving.
        data_dir: PathBuf,
    },
    /// The home folder is unknown, so `~/Library/LaunchAgents` can't be found.
    #[error("the home folder is unknown")]
    NoHomeDir,
    /// The running `wispd`'s own path could not be read.
    #[error("could not find the running wispd's path: {0}")]
    CurrentExe(io::Error),
    /// The data folder could not be prepared. See [`crate::server::prepare_data_dir`].
    #[error(transparent)]
    DataDir(#[from] crate::server::StartError),
    /// `launchctl <argv>` exited with an error.
    #[error("`launchctl {argv}` failed: {stderr}")]
    Launchctl {
        /// The arguments after `launchctl`.
        argv: String,
        /// Its stderr, trimmed.
        stderr: String,
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

impl ServiceError {
    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Where the `LaunchAgent`'s plist goes: `~/Library/LaunchAgents/<label>.plist`.
///
/// # Errors
///
/// [`ServiceError::NoHomeDir`] if the home folder is unknown.
pub fn plist_path(label: &str) -> Result<PathBuf, ServiceError> {
    let home = std::env::home_dir()
        .filter(|home| home.is_absolute())
        .ok_or(ServiceError::NoHomeDir)?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

/// Renders the `LaunchAgent` property list for `label`, running `program serve` with
/// [`DATA_DIR_ENV`] set to `data_dir`'s folder. Both stdout and stderr go to `data_dir`'s
/// `logs/wispd.log` (0009), which `serve` also logs to, so a panic before its own logger starts
/// still lands there instead of being lost.
///
/// `KeepAlive.SuccessfulExit` is `false`: launchd restarts `serve` after it exits with a
/// non-zero status (a crash, or losing the startup race for `wispd.lock`), but not after the
/// exit status of 0 that a clean SIGTERM or SIGINT shutdown produces (`daemon/src/main.rs`), so
/// a deliberate stop stays stopped.
#[must_use]
pub fn render_plist(label: &str, program: &Path, data_dir: &DataDir) -> String {
    let label = escape_plist_text(label);
    let program = escape_plist_text(&program.display().to_string());
    let data_dir_value = escape_plist_text(&data_dir.root().display().to_string());
    let log = escape_plist_text(&data_dir.log_file().display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{program}</string>\n\
         \t\t<string>serve</string>\n\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>{DATA_DIR_ENV}</key>\n\
         \t\t<string>{data_dir_value}</string>\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         \t<key>StandardOutPath</key>\n\
         \t<string>{log}</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t<string>{log}</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

// Only text content is ever generated (never an attribute), so quotes need no escaping.
fn escape_plist_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// What changed when [`install`] ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Nothing was loaded under this label before; it is now installed and started.
    Installed,
    /// This label was already loaded; its plist was refreshed and it was restarted.
    Reinstalled,
}

/// Whether [`uninstall`] found anything to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallOutcome {
    /// The `LaunchAgent` was loaded, stopped, and its plist removed.
    Removed,
    /// Nothing was installed; uninstalling was a no-op.
    NotInstalled,
}

/// Installs or updates the per-user `LaunchAgent` for `label`, running the current `wispd`
/// binary's `serve` against `data_dir`.
///
/// Idempotent: run again, for example after wispd moves, to update the plist and restart the
/// service with the new one (`bootout` then `bootstrap`, since launchd does not reread a
/// bootstrapped job's plist on its own).
///
/// # Errors
///
/// [`ServiceError::AlreadyRunningOutsideLaunchd`] when nothing is loaded under `label` yet, but
/// something is already answering `data_dir`'s socket: bootstrapping would only start a second
/// `serve` that fights the first one for the lock. Stop that `serve` first. Other variants for a
/// filesystem or `launchctl` failure.
pub fn install(label: &str, data_dir: &DataDir) -> Result<InstallOutcome, ServiceError> {
    let uid = rustix::process::getuid().as_raw();
    let already_loaded = load_state(uid, label)?.loaded();
    if !already_loaded && probe_initialize(data_dir) {
        return Err(ServiceError::AlreadyRunningOutsideLaunchd {
            data_dir: data_dir.root().to_owned(),
        });
    }
    crate::server::prepare_data_dir(data_dir.root())?;
    prepare_log_dir(data_dir)?;
    let program = current_exe()?;
    let plist = render_plist(label, &program, data_dir);
    let path = plist_path(label)?;
    write_plist(&path, &plist)?;

    let uid_target = format!("gui/{uid}");
    if already_loaded {
        // --wait: block until the old `serve` is fully gone, so the bootstrap below can never
        // race it for `wispd.lock`.
        launchctl_ok(&["bootout", "--wait", &format!("{uid_target}/{label}")])?;
    }
    launchctl_ok(&["bootstrap", &uid_target, &path.display().to_string()])?;

    Ok(if already_loaded {
        InstallOutcome::Reinstalled
    } else {
        InstallOutcome::Installed
    })
}

/// Removes the per-user `LaunchAgent` for `label`: stops it if loaded, then deletes its plist.
///
/// Idempotent: uninstalling a label that was never installed succeeds and reports
/// [`UninstallOutcome::NotInstalled`].
///
/// # Errors
///
/// A [`ServiceError`] if `launchctl bootout` fails for a reason other than the label already
/// being gone, [`plist_path`] fails, or the plist file exists but can't be removed.
pub fn uninstall(label: &str) -> Result<UninstallOutcome, ServiceError> {
    let uid = rustix::process::getuid().as_raw();
    let was_loaded = load_state(uid, label)?.loaded();
    if was_loaded {
        launchctl_ok(&["bootout", "--wait", &format!("gui/{uid}/{label}")])?;
    }
    let path = plist_path(label)?;
    let removed_file = match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(ServiceError::io(
                format!("removing {}", path.display()),
                error,
            ));
        }
    };
    Ok(if was_loaded || removed_file {
        UninstallOutcome::Removed
    } else {
        UninstallOutcome::NotInstalled
    })
}

/// What launchd reports about a label, from [`load_state`].
///
/// Loaded and running are kept as one enum rather than two bools, so a launchd report can't be
/// misread as the impossible "running but not loaded".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchdState {
    /// launchd does not have the label loaded.
    NotLoaded,
    /// launchd has it loaded, but it is not currently running.
    Loaded,
    /// launchd has it loaded and running, with its pid when it reported one.
    Running(Option<u32>),
}

impl LaunchdState {
    /// launchd has the label loaded in the per-user GUI domain.
    #[must_use]
    pub fn loaded(self) -> bool {
        !matches!(self, Self::NotLoaded)
    }

    /// launchd reports the job as currently running.
    #[must_use]
    pub fn running(self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// The running instance's pid, when launchd reports one.
    #[must_use]
    pub fn pid(self) -> Option<u32> {
        match self {
            Self::Running(pid) => pid,
            Self::NotLoaded | Self::Loaded => None,
        }
    }
}

/// What `wispd service status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The label checked.
    pub label: String,
    /// Where its plist would be.
    pub plist_path: PathBuf,
    /// The plist file exists in `~/Library/LaunchAgents`.
    pub installed: bool,
    /// What launchd reports for `label`.
    pub launchd: LaunchdState,
    /// The data folder's socket answered an `initialize` request just now.
    pub answers_initialize: bool,
}

/// Reports what launchd knows about `label`, and whether `data_dir`'s socket answers right now.
///
/// # Errors
///
/// A [`ServiceError`] if the plist path or `launchctl print` can't be checked. Not answering
/// `initialize` is reported in the result, not treated as an error.
pub fn status(label: &str, data_dir: &DataDir) -> Result<Status, ServiceError> {
    let uid = rustix::process::getuid().as_raw();
    let path = plist_path(label)?;
    let installed = path
        .try_exists()
        .map_err(|error| ServiceError::io(format!("checking {}", path.display()), error))?;
    let launchd = load_state(uid, label)?;
    let answers_initialize = probe_initialize(data_dir);
    Ok(Status {
        label: label.to_owned(),
        plist_path: path,
        installed,
        launchd,
        answers_initialize,
    })
}

/// Asks launchd about `label` with `launchctl print`. A label nothing has bootstrapped yet is
/// not an error: `print` simply exits non-zero, which reads as [`LaunchdState::NotLoaded`].
fn load_state(uid: u32, label: &str) -> Result<LaunchdState, ServiceError> {
    let output = run_launchctl(&["print", &format!("gui/{uid}/{label}")])?;
    if !output.status.success() {
        return Ok(LaunchdState::NotLoaded);
    }
    let (running, pid) = parse_print_output(&String::from_utf8_lossy(&output.stdout));
    Ok(if running {
        LaunchdState::Running(pid)
    } else {
        LaunchdState::Loaded
    })
}

/// Reads `state = running` and `pid = <n>` out of `launchctl print`'s text. Anything else about
/// a not-currently-running job (`launchctl` has used both "not running" and no `state` line at
/// all across macOS releases) is left as `running: false, pid: None`, which is always correct
/// even if the exact wording changes again.
fn parse_print_output(text: &str) -> (bool, Option<u32>) {
    let running = text.lines().any(|line| line.trim() == "state = running");
    let pid = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = ")?.trim().parse().ok());
    (running, pid)
}

/// Connects to `data_dir`'s socket and sends `initialize`, to see whether something answers it
/// right now. `false` covers every way that can fail to happen: no socket, nothing listening, a
/// connection that doesn't answer in time, or an answer that isn't a well-formed response.
fn probe_initialize(data_dir: &DataDir) -> bool {
    let Ok(socket) = data_dir.socket_path() else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(&socket.path) else {
        return false;
    };
    if stream.set_read_timeout(Some(PROBE_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(PROBE_TIMEOUT)).is_err()
    {
        return false;
    }
    let request = Request::new::<Initialize>(
        1,
        InitializeParams {
            protocol: ProtocolRange::SUPPORTED,
            client: ClientInfo {
                name: "wispd-service".to_owned(),
                version: VERSION.to_owned(),
                machine_id: None,
            },
            capabilities: Capabilities::default(),
        },
    );
    let Ok(mut line) = serde_json::to_string(&request) else {
        return false;
    };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    if reader.read_line(&mut response_line).is_err() {
        return false;
    }
    let frame = response_line.trim_end_matches(['\n', '\r']);
    matches!(
        Message::from_frame(frame.as_bytes()),
        Ok(Message::Response(_))
    )
}

/// Creates `logs/` under the data folder, if it is not there yet, so launchd has somewhere to
/// point `StandardOutPath` and `StandardErrorPath` before `serve` itself creates the folder.
fn prepare_log_dir(data_dir: &DataDir) -> Result<(), ServiceError> {
    let log_file = data_dir.log_file();
    let Some(dir) = log_file.parent() else {
        return Ok(());
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| ServiceError::io(format!("creating {}", dir.display()), error))
}

/// The absolute path of the running `wispd` binary, as `std::env::current_exe` reports it.
///
/// This is not resolved through symlinks: a package manager that upgrades wispd by relinking a
/// stable path should keep the plist pointing at that stable path. Re-running `install` after
/// moving the binary picks up its new location either way.
fn current_exe() -> Result<PathBuf, ServiceError> {
    std::env::current_exe().map_err(ServiceError::CurrentExe)
}

fn write_plist(path: &Path, contents: &str) -> Result<(), ServiceError> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .create(dir)
            .map_err(|error| ServiceError::io(format!("creating {}", dir.display()), error))?;
    }
    std::fs::write(path, contents)
        .map_err(|error| ServiceError::io(format!("writing {}", path.display()), error))
}

fn run_launchctl(args: &[&str]) -> Result<Output, ServiceError> {
    Command::new(LAUNCHCTL)
        .args(args)
        .output()
        .map_err(|error| ServiceError::io(format!("running launchctl {}", args.join(" ")), error))
}

/// Runs `launchctl` and turns a non-zero exit into [`ServiceError::Launchctl`].
fn launchctl_ok(args: &[&str]) -> Result<Output, ServiceError> {
    let output = run_launchctl(args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(ServiceError::Launchctl {
            argv: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DEFAULT_LABEL, escape_plist_text, parse_print_output, plist_path, render_plist};
    use crate::paths::DataDir;

    #[test]
    fn plist_matches_the_golden_file() {
        let data_dir = DataDir::new("/Users/ryan/Library/Application Support/wisp").unwrap();
        let plist = render_plist(DEFAULT_LABEL, Path::new("/usr/local/bin/wispd"), &data_dir);
        let golden = include_str!("../tests/golden/launchagent.plist");
        assert_eq!(plist, golden);
    }

    #[test]
    fn special_characters_in_paths_and_the_label_are_escaped() {
        let data_dir = DataDir::new("/Users/a&b/wisp").unwrap();
        let plist = render_plist("a&b<c>", Path::new("/bin/<wispd>"), &data_dir);
        assert!(
            plist.contains("<string>a&amp;b&lt;c&gt;</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/bin/&lt;wispd&gt;</string>"),
            "{plist}"
        );
        assert!(plist.contains("/Users/a&amp;b/wisp"), "{plist}");
    }

    #[test]
    fn escaping_handles_the_reserved_characters_and_leaves_the_rest_alone() {
        assert_eq!(escape_plist_text("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(escape_plist_text("plain"), "plain");
    }

    #[test]
    fn plist_path_is_under_launch_agents_with_the_label() {
        let path = plist_path("io.example.test").unwrap();
        assert!(
            path.ends_with("Library/LaunchAgents/io.example.test.plist"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn parses_a_running_jobs_state_and_pid_from_launchctl_print() {
        // A trimmed capture of real `launchctl print gui/<uid>/<label>` output.
        let text = "gui/501/io.example = {\n\
                     \tactive count = 1\n\
                     \tstate = running\n\n\
                     \tprogram = /bin/sleep\n\
                     \tpid = 85309\n\
                     \truns = 1\n\
                     }\n";
        assert_eq!(parse_print_output(text), (true, Some(85309)));
    }

    #[test]
    fn a_job_that_is_not_running_has_no_pid() {
        let text = "gui/501/io.example = {\n\tstate = not running\n}\n";
        assert_eq!(parse_print_output(text), (false, None));
    }

    #[test]
    fn unrecognized_output_is_read_as_not_running_rather_than_an_error() {
        assert_eq!(parse_print_output(""), (false, None));
    }
}
