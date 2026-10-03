//! Installing from a fake release of `file://` URLs, and signing in a fake `gh` (PLX-423). Nothing
//! here reaches the network or runs the real gh.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use parallax_protocol::GithubStatus;
use serde_json::json;
use tempfile::TempDir;

use super::{Github, asset_name, bin_dir, device_code, sha256_hex, with_tools_on_path};
use crate::backend::process::{Environment, Launcher, find_program};
use crate::detect::CliDetector;
use crate::paths::DataDir;

const FAKE_GH: &str = include_str!("../detect/fixtures/fake-gh.sh");

/// A data folder, a `bin` folder on `PATH`, and a fake release.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(fixture.bin()).unwrap();
        fs::create_dir(fixture.path("release")).unwrap();
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join(name)
    }

    fn bin(&self) -> PathBuf {
        self.path("bin")
    }

    fn data_dir(&self) -> DataDir {
        DataDir::new(self.path("data")).unwrap()
    }

    fn tools(&self) -> PathBuf {
        self.data_dir().tools_dir()
    }

    /// Writes `script` to `dir/name`, executable.
    fn script(dir: &Path, name: &str, script: &str) {
        let program = dir.join(name);
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Links the system's `name` into `bin`, so `PATH` can hold it without the system's `gh`.
    fn link(&self, name: &str) {
        let found = find_program(name.as_ref(), Some("/usr/bin:/bin".as_ref())).unwrap();
        std::os::unix::fs::symlink(found, self.bin().join(name)).unwrap();
    }

    /// A launcher whose `PATH` is `path`, then plxd's tools folder, as `serve` sets it up.
    fn launcher(&self, path: &str, vars: &[(&str, &str)]) -> Launcher {
        let mut env = Environment::empty();
        env.set("PATH", path);
        for (name, value) in vars {
            env.set(*name, *value);
        }
        let data_dir = self.data_dir();
        Launcher::new(data_dir.clone(), with_tools_on_path(env, &data_dir))
    }

    /// A release of gh 9.9.9 for this platform, whose `gh` is the fake one, and the URL of its
    /// description. `checksum` replaces the archive's real SHA-256 when given.
    fn release(&self, checksum: Option<&str>) -> String {
        let release = self.path("release");
        let name = asset_name("9.9.9", std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let top = name.trim_end_matches(".zip").trim_end_matches(".tar.gz");
        let bin = release.join("build").join(top).join("bin");
        fs::create_dir_all(&bin).unwrap();
        Self::script(&bin, "gh", &FAKE_GH.replace("2.100.0", "9.9.9"));
        let format: &[&str] = if Path::new(&name).extension().is_some_and(|e| e == "zip") {
            &["--format", "zip", "-cf"]
        } else {
            &["-czf"]
        };
        let packed = std::process::Command::new("tar")
            .args(format)
            .arg(release.join(&name))
            .arg("-C")
            .arg(release.join("build"))
            .arg(top)
            .status()
            .unwrap();
        assert!(packed.success());
        let real = sha256_hex(&fs::read(release.join(&name)).unwrap());
        fs::write(
            release.join("gh_9.9.9_checksums.txt"),
            format!(
                "{}  gh_9.9.9_linux_386.tar.gz\n{}  {name}\n",
                "0".repeat(64),
                checksum.unwrap_or(&real)
            ),
        )
        .unwrap();
        let url = |file: &str| format!("file://{}", release.join(file).display());
        let description = json!({
            "tag_name": "v9.9.9",
            "assets": [
                {"name": "gh_9.9.9_checksums.txt", "browser_download_url": url("gh_9.9.9_checksums.txt")},
                {"name": name, "browser_download_url": url(&name)},
            ],
        });
        fs::write(release.join("latest.json"), description.to_string()).unwrap();
        url("latest.json")
    }
}

/// Polls `github`'s state until `done` holds for it, for at most 30 s.
async fn wait(github: &Github, done: impl Fn(&GithubStatus) -> bool) -> GithubStatus {
    for _ in 0..300 {
        let mut status = blank();
        github.fill(&mut status);
        if done(&status) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("github setup never got there");
}

fn blank() -> GithubStatus {
    GithubStatus {
        installed: false,
        version: None,
        signed_in: None,
        account: None,
        note: None,
        managed: false,
        installing: false,
        signing_in: None,
        setup_note: None,
        checked_at: jiff::Timestamp::now(),
    }
}

/// What's left in the tools folder.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn install_puts_a_verified_gh_in_the_tools_folder_where_status_finds_it() {
    let fixture = Fixture::new();
    fixture.link("curl");
    fixture.link("tar");
    // GNU tar runs gzip to unpack Linux's tar.gz.
    fixture.link("gzip");
    let launcher = fixture.launcher(&fixture.bin().display().to_string(), &[]);
    let github = Github::new(launcher.clone(), fixture.release(None));
    github.install().unwrap();
    let mut status = blank();
    github.fill(&mut status);
    assert!(status.installing);
    let status = wait(&github, |status| !status.installing).await;
    assert_eq!(status.setup_note, None);
    assert_eq!(listing(&fixture.tools()), ["gh"]);
    assert!(bin_dir(&fixture.data_dir()).join("gh").is_file());

    let status = CliDetector::new(launcher, Duration::from_secs(5))
        .github()
        .await;
    assert!(status.installed, "{status:?}");
    assert!(status.managed, "{status:?}");
    assert_eq!(status.version.as_deref(), Some("9.9.9"));
    // Now that one is found, Install is refused.
    assert!(github.install().unwrap_err().contains("already installed"));
}

#[tokio::test]
async fn a_checksum_mismatch_installs_nothing() {
    let fixture = Fixture::new();
    fixture.link("curl");
    fixture.link("tar");
    let launcher = fixture.launcher(&fixture.bin().display().to_string(), &[]);
    let github = Github::new(launcher, fixture.release(Some(&"a".repeat(64))));
    github.install().unwrap();
    let status = wait(&github, |status| !status.installing).await;
    let note = status.setup_note.unwrap();
    assert!(note.contains("doesn't match gh's checksum"), "{note}");
    assert_eq!(listing(&fixture.tools()), Vec::<String>::new());
}

#[tokio::test]
async fn a_missing_curl_or_a_failed_download_installs_nothing() {
    let fixture = Fixture::new();
    fixture.link("tar");
    let launcher = fixture.launcher(&fixture.bin().display().to_string(), &[]);
    let github = Github::new(launcher.clone(), fixture.release(None));
    github.install().unwrap();
    let note = wait(&github, |status| !status.installing)
        .await
        .setup_note
        .unwrap();
    assert!(note.contains("curl was not found"), "{note}");
    assert_eq!(listing(&fixture.tools()), Vec::<String>::new());

    fixture.link("curl");
    let missing = format!("file://{}", fixture.path("release/none.json").display());
    let github = Github::new(launcher, missing);
    github.install().unwrap();
    let note = wait(&github, |status| !status.installing)
        .await
        .setup_note
        .unwrap();
    assert!(
        note.starts_with("Couldn't download the latest gh release:"),
        "{note}"
    );
    assert_eq!(listing(&fixture.tools()), Vec::<String>::new());
}

#[tokio::test]
async fn a_gh_on_the_users_path_wins_over_plxds_and_install_is_refused() {
    let fixture = Fixture::new();
    let tools_bin = bin_dir(&fixture.data_dir());
    fs::create_dir_all(&tools_bin).unwrap();
    Fixture::script(&tools_bin, "gh", &FAKE_GH.replace("2.100.0", "9.9.9"));
    Fixture::script(&fixture.bin(), "gh", FAKE_GH);
    let launcher = fixture.launcher(&fixture.bin().display().to_string(), &[]);
    let status = CliDetector::new(launcher.clone(), Duration::from_secs(5))
        .github()
        .await;
    assert_eq!(status.version.as_deref(), Some("2.100.0"));
    assert!(!status.managed);
    let github = Github::new(launcher, "file:///nonexistent");
    assert!(github.install().unwrap_err().contains("already installed"));
}

#[test]
fn the_tools_folder_goes_last_on_path() {
    let data_dir = DataDir::new("/data").unwrap();
    let mut env = Environment::empty();
    env.set("PATH", "/opt/homebrew/bin:/usr/bin");
    let env = with_tools_on_path(env, &data_dir);
    assert_eq!(
        env.get("PATH").unwrap(),
        "/opt/homebrew/bin:/usr/bin:/data/tools/gh/bin"
    );
}

#[test]
fn the_code_is_read_from_either_of_ghs_wordings() {
    assert_eq!(
        device_code("! One-time code (AA17-58F5) copied to clipboard").as_deref(),
        Some("AA17-58F5")
    );
    assert_eq!(
        device_code("! First copy your one-time code: 0A1B-C2D3").as_deref(),
        Some("0A1B-C2D3")
    );
    assert_eq!(
        device_code("Open this URL to continue: https://github.com/login/device"),
        None
    );
}

/// A launcher with the fake `gh` first on `PATH`, and `vars` for it.
fn sign_in_launcher(fixture: &Fixture, vars: &[(&str, &str)]) -> Launcher {
    Fixture::script(&fixture.bin(), "gh", FAKE_GH);
    fixture.launcher(&format!("{}:/bin:/usr/bin", fixture.bin().display()), vars)
}

#[tokio::test]
async fn sign_in_shows_the_code_then_runs_setup_git_once_approved() {
    let fixture = Fixture::new();
    let approve = fixture.path("approve");
    let mark = fixture.path("setup-git-ran");
    let launcher = sign_in_launcher(
        &fixture,
        &[
            ("FAKE_GH_APPROVE", &approve.display().to_string()),
            ("FAKE_GH_SETUP_GIT_MARK", &mark.display().to_string()),
        ],
    );
    let github = Github::new(launcher, "file:///nonexistent");
    let shown = github.sign_in().await.unwrap();
    assert_eq!(shown.code, "AB12-CD34");
    assert_eq!(shown.url, "https://github.com/login/device");
    // A second Sign in gets the same code rather than a second gh.
    assert_eq!(github.sign_in().await.unwrap(), shown);
    let mut status = blank();
    github.fill(&mut status);
    assert_eq!(status.signing_in, Some(shown));

    fs::write(&approve, "").unwrap();
    let status = wait(&github, |status| status.signing_in.is_none()).await;
    assert_eq!(status.setup_note, None);
    assert!(mark.exists(), "gh auth setup-git never ran");
}

#[tokio::test]
async fn a_setup_git_failure_is_a_note_after_a_sign_in_that_worked() {
    let fixture = Fixture::new();
    let approve = fixture.path("approve");
    fs::write(&approve, "").unwrap();
    let launcher = sign_in_launcher(
        &fixture,
        &[
            ("FAKE_GH_APPROVE", &approve.display().to_string()),
            ("FAKE_GH_SETUP_GIT_EXIT", "1"),
        ],
    );
    let github = Github::new(launcher, "file:///nonexistent");
    github.sign_in().await.unwrap();
    let note = wait(&github, |status| status.signing_in.is_none())
        .await
        .setup_note
        .unwrap();
    assert_eq!(
        note,
        "Signed in, but `gh auth setup-git` failed, so git can't use this sign-in to push over \
         HTTPS: failed to set up git credential helper: read-only file system"
    );
}

#[tokio::test]
async fn a_failed_sign_in_says_why() {
    let fixture = Fixture::new();
    let launcher = sign_in_launcher(&fixture, &[("FAKE_GH_LOGIN", "fail")]);
    let github = Github::new(launcher, "file:///nonexistent");
    // gh may exit before plxd reads its code, or after.
    let note = match github.sign_in().await {
        Ok(_) => wait(&github, |status| status.signing_in.is_none())
            .await
            .setup_note
            .unwrap(),
        Err(why) => why,
    };
    assert!(note.ends_with("error: the device code expired"), "{note}");
}

#[tokio::test]
async fn cancel_stops_gh_and_clears_the_sign_in() {
    let fixture = Fixture::new();
    let pidfile = fixture.path("gh.pid");
    let launcher = sign_in_launcher(
        &fixture,
        &[("FAKE_GH_PIDFILE", &pidfile.display().to_string())],
    );
    let github = Github::new(launcher, "file:///nonexistent");
    github.sign_in().await.unwrap();
    let pid: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive =
        || rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok();
    assert!(alive());

    github.cancel_sign_in();
    let status = wait(&github, |status| status.signing_in.is_none()).await;
    assert_eq!(status.setup_note, None);
    for _ in 0..50 {
        if !alive() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("gh auth login outlived its cancel");
}
