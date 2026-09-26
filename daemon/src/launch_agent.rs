//! Starting the launch agent that [`crate::service`] installs (#61), as `attach` does when it
//! finds wispd not running (0010).
//!
//! The label and plist path are [`service`]'s. The agent under [`DEFAULT_LABEL`] serves the
//! default data folder, which `wispd service install` enforces.

use std::io::{self, Read as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::paths::DataDir;
use crate::service::{self, DEFAULT_LABEL, LAUNCHCTL};

const POLL: Duration = Duration::from_millis(10);

/// A launch agent that `attach` can start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchAgent {
    /// The `launchctl` to run: `/bin/launchctl`, except in tests.
    pub launchctl: PathBuf,
    /// The service target to start.
    pub service: String,
}

impl LaunchAgent {
    /// This user's launch agent, if it is installed and serves `data_dir`.
    ///
    /// The agent serves the default data folder, so a `--data-dir` or `WISPD_DATA_DIR` that
    /// names another folder never starts it. It counts as installed when its plist exists.
    #[must_use]
    pub fn installed_for(data_dir: &DataDir) -> Option<Self> {
        if DataDir::default_location().ok()? != *data_dir {
            return None;
        }
        service::plist_path(DEFAULT_LABEL)
            .ok()?
            .is_file()
            .then(|| Self {
                launchctl: PathBuf::from(LAUNCHCTL),
                // The user's GUI domain, where the Keychain is reachable (0004).
                service: format!("gui/{}/{DEFAULT_LABEL}", rustix::process::getuid().as_raw()),
            })
    }

    /// Starts the service unless it is running, with `launchctl kickstart`, waiting for
    /// `launchctl` until `deadline` at the latest.
    ///
    /// # Errors
    ///
    /// If `launchctl` can't run, fails, or is still running at `deadline`, in which case it is
    /// killed. The error includes what it printed on stderr.
    pub fn kickstart(&self, deadline: Instant) -> io::Result<()> {
        let mut child = Command::new(&self.launchctl)
            .arg("kickstart")
            .arg(&self.service)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "`launchctl kickstart {}` did not finish in time",
                        self.service
                    ),
                ));
            }
            thread::sleep(POLL.min(deadline - now));
        };
        if status.success() {
            return Ok(());
        }
        // launchctl prints a line or two, well under a pipe's buffer, so it never blocked on
        // writing it.
        let mut printed = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut printed);
        }
        Err(io::Error::other(format!(
            "`launchctl kickstart {}` failed ({status}): {}",
            self.service,
            printed.trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::LaunchAgent;
    use crate::paths::DataDir;

    #[test]
    fn another_data_folder_never_uses_the_launch_agent() {
        let dir = DataDir::new("/tmp/wispd-not-the-default").unwrap();
        assert_eq!(LaunchAgent::installed_for(&dir), None);
    }

    fn fake_launchctl(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("launchctl");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn agent(launchctl: PathBuf) -> LaunchAgent {
        LaunchAgent {
            launchctl,
            service: "gui/501/test".to_owned(),
        }
    }

    #[test]
    fn a_failed_kickstart_reports_what_launchctl_printed() {
        let dir = tempfile::tempdir().unwrap();
        let failing = agent(fake_launchctl(
            dir.path(),
            "echo 'Could not find service' >&2; exit 113",
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        let error = failing.kickstart(deadline).unwrap_err().to_string();
        assert!(error.contains("Could not find service"), "{error}");
        assert!(error.contains("113"), "{error}");
    }

    #[test]
    fn a_kickstart_that_hangs_is_given_up_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let hanging = agent(fake_launchctl(dir.path(), "exec sleep 30"));
        let started = Instant::now();
        let error = hanging
            .kickstart(started + Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }
}
