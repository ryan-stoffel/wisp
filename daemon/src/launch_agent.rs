//! The launch agent that keeps `wispd serve` running on a host (#61), as far as `attach` needs it:
//! its label, where its plist is installed, and how to start it (0010).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::paths::DataDir;

/// The launch agent's label: the app's bundle id (0006) and `.wispd`.
pub const LABEL: &str = "io.github.ryan-stoffel.wisp.wispd";

const LAUNCHCTL: &str = "/bin/launchctl";

/// Where the launch agent's plist is installed, for the user whose home folder is `home`.
#[must_use]
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The launch agent's service target for the user `uid`: `gui/<uid>/<LABEL>`. launchd loads the
/// agent into the user's GUI domain, where the Keychain is reachable (0004).
#[must_use]
pub fn service_target(uid: u32) -> String {
    format!("gui/{uid}/{LABEL}")
}

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
    /// The launch agent serves the default data folder, so a `--data-dir` or `WISPD_DATA_DIR`
    /// that names another folder never starts it. It counts as installed when its plist exists.
    #[must_use]
    pub fn installed_for(data_dir: &DataDir) -> Option<Self> {
        if DataDir::default_location().ok()? != *data_dir {
            return None;
        }
        let home = std::env::home_dir()?;
        plist_path(&home).is_file().then(|| Self {
            launchctl: PathBuf::from(LAUNCHCTL),
            service: service_target(rustix::process::getuid().as_raw()),
        })
    }

    /// Starts the service unless it is running, with `launchctl kickstart`.
    ///
    /// # Errors
    ///
    /// If `launchctl` can't run or fails. The error includes what it printed.
    pub fn kickstart(&self) -> io::Result<()> {
        let output = Command::new(&self.launchctl)
            .arg("kickstart")
            .arg(&self.service)
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        let printed = if output.stderr.trim_ascii().is_empty() {
            output.stdout
        } else {
            output.stderr
        };
        Err(io::Error::other(format!(
            "`launchctl kickstart {}` failed ({}): {}",
            self.service,
            output.status,
            String::from_utf8_lossy(printed.trim_ascii())
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::{LaunchAgent, plist_path, service_target};
    use crate::paths::DataDir;

    #[test]
    fn the_label_names_the_plist_and_the_service() {
        assert_eq!(
            plist_path(Path::new("/Users/me")),
            Path::new("/Users/me/Library/LaunchAgents/io.github.ryan-stoffel.wisp.wispd.plist")
        );
        assert_eq!(
            service_target(501),
            "gui/501/io.github.ryan-stoffel.wisp.wispd"
        );
    }

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

    #[test]
    fn kickstart_runs_launchctl_and_reports_its_failure() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        let ok = LaunchAgent {
            launchctl: fake_launchctl(dir.path(), &format!("echo \"$@\" > '{}'", args.display())),
            service: "gui/501/test".to_owned(),
        };
        ok.kickstart().unwrap();
        assert_eq!(
            fs::read_to_string(&args).unwrap(),
            "kickstart gui/501/test\n"
        );

        let failing = LaunchAgent {
            launchctl: fake_launchctl(dir.path(), "echo 'Could not find service' >&2; exit 113"),
            service: "gui/501/test".to_owned(),
        };
        let error = failing.kickstart().unwrap_err().to_string();
        assert!(error.contains("Could not find service"), "{error}");
        assert!(error.contains("113"), "{error}");
    }
}
