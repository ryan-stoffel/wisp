//! Setting up the GitHub CLI on the host (PLX-423, decision record 0050): `github/install` and
//! `github/signIn`.
//!
//! - **Install** downloads the latest `cli/cli` release's archive for this OS and arch with the
//!   system `curl`, checks its SHA-256 against the release's `checksums.txt`, unpacks it with the
//!   system `tar` (bsdtar reads the macOS and Windows zips), and renames it into
//!   `<data>/tools/gh/`. Everything before the rename happens in a temp folder inside `tools/`,
//!   so a failure leaves nothing behind. It runs in the background, since it can take longer
//!   than a request may, and `github/status` reports it. [`with_tools_on_path`] puts
//!   `tools/gh/bin` at the end of the launcher's `PATH`, so a `gh` the user installed wins.
//! - **Sign-in** runs `gh auth login --web`, which prints a one-time code and GitHub's device
//!   page to stderr and then waits for the user to approve it in a browser. plxd answers with the
//!   code and keeps the process running, one per host, until it exits, the code expires, or
//!   `github/signInCancel` kills its process group. After a sign-in that worked, it runs `gh auth
//!   setup-git`, whose failure is a note, not an error. `gh` keeps the token; plxd never reads it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use parallax_protocol::{GithubSignIn, GithubStatus};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::backend::process::{Environment, Launcher, Output, Process, Signal, Signals};
use crate::detect::{probe_spec, resolve, run_spec};
use crate::paths::DataDir;

/// The latest `cli/cli` release, as GitHub's API describes it.
pub const RELEASE_URL: &str = "https://api.github.com/repos/cli/cli/releases/latest";

/// How long the release's description and checksums may take to download.
const SMALL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the archive (about 15 MB) may take to download, or to unpack.
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(600);

/// How long `gh auth login` may take to print its code, under the app's 30 s request timeout.
const CODE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a device code lasts on GitHub, after which plxd stops waiting.
const CODE_LIFETIME: Duration = Duration::from_mins(15);

/// How long `gh auth setup-git` may take.
const SETUP_GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `<data>/tools/gh/bin`, where an installed `gh` is.
pub(crate) fn bin_dir(data_dir: &DataDir) -> PathBuf {
    data_dir.tools_dir().join("gh").join("bin")
}

/// `env` with [`bin_dir`] at the end of its `PATH`, so every process plxd starts, threads' CLIs
/// and PR actions included, finds an installed `gh`, after any the user installed.
pub(crate) fn with_tools_on_path(mut env: Environment, data_dir: &DataDir) -> Environment {
    let mut dirs: Vec<PathBuf> = env
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    dirs.push(bin_dir(data_dir));
    if let Ok(path) = std::env::join_paths(dirs) {
        env.set("PATH", path);
    }
    env
}

/// The host's GitHub setup: an install or a sign-in in progress, and how the last one went.
#[derive(Debug)]
pub(crate) struct Github {
    launcher: Launcher,
    release_url: String,
    state: Arc<Mutex<State>>,
    /// Held while a sign-in starts, so two requests start one `gh auth login`.
    starting: tokio::sync::Mutex<()>,
}

#[derive(Debug, Default)]
struct State {
    installing: bool,
    sign_in: Option<Pending>,
    /// The protocol's `setupNote`.
    note: Option<String>,
    /// Counts sign-ins, so one that was cancelled doesn't clear the next.
    started: u64,
}

#[derive(Debug)]
struct Pending {
    shown: GithubSignIn,
    signals: Signals,
    id: u64,
}

impl Github {
    /// Setup through `launcher`, installing from the release `release_url` describes:
    /// [`RELEASE_URL`], except in tests.
    pub(crate) fn new(launcher: Launcher, release_url: impl Into<String>) -> Self {
        Self {
            launcher,
            release_url: release_url.into(),
            state: Arc::default(),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Adds what's in progress, and the last note, to a probe's `status`.
    pub(crate) fn fill(&self, status: &mut GithubStatus) {
        let state = self.state();
        status.installing = state.installing;
        status.signing_in = state.sign_in.as_ref().map(|pending| pending.shown.clone());
        status.setup_note.clone_from(&state.note);
    }

    /// Starts installing `gh` in the background, unless it's being installed already.
    ///
    /// # Errors
    ///
    /// When a `gh` is found on the launcher's `PATH`.
    pub(crate) fn install(&self) -> Result<(), String> {
        if let Some(path) = resolve(&self.launcher, "gh") {
            return Err(format!("gh is already installed at {}.", path.display()));
        }
        {
            let mut state = self.state();
            if state.installing {
                return Ok(());
            }
            state.installing = true;
            state.note = None;
        }
        let launcher = self.launcher.clone();
        let url = self.release_url.clone();
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let installed = install(&launcher, &url).await;
            let mut state = lock(&state);
            state.installing = false;
            state.note = installed.err();
        });
        Ok(())
    }

    /// Starts `gh auth login --web` and answers with its one-time code, or with the pending
    /// sign-in's.
    ///
    /// # Errors
    ///
    /// When `gh` can't start, or exits or goes quiet before it prints a code.
    pub(crate) async fn sign_in(&self) -> Result<GithubSignIn, String> {
        let _starting = self.starting.lock().await;
        if let Some(pending) = &self.state().sign_in {
            return Ok(pending.shown.clone());
        }
        let (process, code, url) = start_login(&self.launcher).await?;
        let lifetime = SignedDuration::try_from(CODE_LIFETIME).unwrap_or_default();
        let shown = GithubSignIn {
            code,
            url,
            expires_at: Timestamp::now()
                .checked_add(lifetime)
                .unwrap_or(Timestamp::MAX),
        };
        let id = {
            let mut state = self.state();
            state.started += 1;
            state.note = None;
            state.sign_in = Some(Pending {
                shown: shown.clone(),
                signals: process.signals().clone(),
                id: state.started,
            });
            state.started
        };
        tokio::spawn(finish_login(
            process,
            self.launcher.clone(),
            Arc::clone(&self.state),
            id,
        ));
        Ok(shown)
    }

    /// Stops the pending sign-in, if there is one, killing `gh`'s process group.
    pub(crate) fn cancel_sign_in(&self) {
        if let Some(pending) = self.state().sign_in.take() {
            let _ = pending.signals.signal_group(Signal::KILL);
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts `gh auth login --web`, and reads its one-time code and device page from its stderr.
async fn start_login(launcher: &Launcher) -> Result<(Process, String, String), String> {
    let mut spec = probe_spec("gh");
    spec.args = [
        "auth",
        "login",
        "--hostname",
        "github.com",
        "--git-protocol",
        "https",
        "--web",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    // Login has to print, and the code needs no color codes around it.
    spec.scrub = vec!["GH_PROMPT_DISABLED".into()];
    spec.inject.set("NO_COLOR", "1");
    spec.stderr_lines = true;
    let mut process = launcher
        .spawn(&spec)
        .map_err(|error| format!("Couldn't start gh auth login: {error}."))?;
    let stopped = |why: &str| format!("gh auth login stopped before it showed a code: {why}");
    let read = tokio::time::timeout(CODE_TIMEOUT, async {
        let (mut code, mut url) = (None, None);
        while let Some(output) = process.next().await {
            match output {
                Output::Line(bytes) => {
                    let line = String::from_utf8_lossy(&bytes);
                    code = code.or_else(|| device_code(&line));
                    url = url.or_else(|| device_url(&line));
                    if let (Some(code), Some(url)) = (&code, &url) {
                        return Ok((code.clone(), url.clone()));
                    }
                }
                Output::Oversized { .. } => {}
                Output::Exited(exit) => return Err(stopped(last_line(&exit.stderr_tail))),
            }
        }
        Err(stopped("it printed nothing"))
    })
    .await;
    match read {
        Ok(read) => read.map(|(code, url)| (process, code, url)),
        Err(_) => Err(format!(
            "gh auth login showed no code within {}s.",
            CODE_TIMEOUT.as_secs()
        )),
    }
}

/// Waits for sign-in `id`'s `gh auth login` to exit, at most until its code expires, then runs
/// `gh auth setup-git` if it worked, and clears the sign-in unless it was cancelled meanwhile.
async fn finish_login(mut process: Process, launcher: Launcher, state: Arc<Mutex<State>>, id: u64) {
    let exited = tokio::time::timeout(CODE_LIFETIME, async {
        loop {
            match process.next().await {
                Some(Output::Exited(exit)) => break Some(exit),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await;
    let note = match exited {
        Ok(Some(exit)) if exit.info.success() => setup_git(&launcher).await.err(),
        Ok(Some(exit)) => Some(format!("Sign-in failed: {}", last_line(&exit.stderr_tail))),
        Ok(None) => Some("Sign-in failed.".to_owned()),
        Err(_) => Some("The code expired before it was entered. Sign in again.".to_owned()),
    };
    // Kills `gh` if the code expired.
    drop(process);
    let mut state = lock(&state);
    if state
        .sign_in
        .as_ref()
        .is_some_and(|pending| pending.id == id)
    {
        state.sign_in = None;
        state.note = note;
    }
}

/// `bytes`' SHA-256, in lowercase hex as `checksums.txt` has it.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// The last non-empty line of `text`, or a stand-in when there is none.
fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it printed nothing")
}

/// The one-time code in a line of `gh auth login`'s: `! One-time code (AA17-58F5) copied to
/// clipboard`, or `! First copy your one-time code: AA17-58F5` when the copy fails.
fn device_code(line: &str) -> Option<String> {
    if !line.contains("one-time code") && !line.contains("One-time code") {
        return None;
    }
    line.split(|c: char| c.is_whitespace() || "():".contains(c))
        .find(|word| {
            let mut halves = word.split('-');
            let four = |half: Option<&str>| {
                half.is_some_and(|h| h.len() == 4 && h.bytes().all(|b| b.is_ascii_alphanumeric()))
            };
            four(halves.next()) && four(halves.next()) && halves.next().is_none()
        })
        .map(str::to_owned)
}

/// The page in a line of `gh auth login`'s: `Open this URL to continue in your web browser:
/// https://github.com/login/device`.
fn device_url(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|word| word.starts_with("https://"))
        .map(str::to_owned)
}

/// Runs `gh auth setup-git`, so git pushes over HTTPS with gh's sign-in.
async fn setup_git(launcher: &Launcher) -> Result<(), String> {
    let mut spec = probe_spec("gh");
    spec.args = ["auth", "setup-git", "--hostname", "github.com"]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.inject.set("GH_PROMPT_DISABLED", "1");
    let failed = |why: &str| {
        format!(
            "Signed in, but `gh auth setup-git` failed, so git can't use this sign-in to push over HTTPS: {why}"
        )
    };
    let ran = run_spec(launcher, &spec, b"", SETUP_GIT_TIMEOUT)
        .await
        .map_err(|error| failed(&error))?;
    if ran.exit_code == Some(0) {
        Ok(())
    } else {
        Err(failed(last_line(&ran.stderr_tail)))
    }
}

/// What plxd reads of a release.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The archive's name in a `cli/cli` release for `os` and `arch` as Rust names them, such as
/// `gh_2.102.0_macOS_arm64.zip`. `None` for a platform gh has no build for.
fn asset_name(version: &str, os: &str, arch: &str) -> Option<String> {
    let (os, extension) = match os {
        "macos" => ("macOS", "zip"),
        "linux" => ("linux", "tar.gz"),
        "windows" => ("windows", "zip"),
        _ => return None,
    };
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => return None,
    };
    Some(format!("gh_{version}_{os}_{arch}.{extension}"))
}

/// Installs the release `release_url` describes into `<data>/tools/gh/`. Returns what went wrong,
/// for people.
async fn install(launcher: &Launcher, release_url: &str) -> Result<(), String> {
    let tools = launcher.data_dir().tools_dir();
    std::fs::create_dir_all(&tools)
        .map_err(|error| format!("Couldn't make {}: {error}.", tools.display()))?;
    // Removed on drop, with whatever is still in it.
    let temp = tempfile::Builder::new()
        .prefix(".gh-")
        .tempdir_in(&tools)
        .map_err(|error| {
            format!(
                "Couldn't make a temp folder in {}: {error}.",
                tools.display()
            )
        })?;
    let dir = temp.path();
    let name = download_release(launcher, release_url, dir).await?;
    let checksums = std::fs::read_to_string(dir.join("checksums.txt")).unwrap_or_default();
    let expected = checksums
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(char::is_whitespace)?;
            (file.trim() == name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("gh's checksums don't list {name}."))?;
    let bytes = std::fs::read(dir.join(&name))
        .map_err(|error| format!("Couldn't read the downloaded {name}: {error}."))?;
    if sha256_hex(&bytes) != expected {
        return Err(format!(
            "The downloaded {name} doesn't match gh's checksum, so plxd didn't install it."
        ));
    }

    let unpacked = dir.join("unpacked");
    std::fs::create_dir(&unpacked)
        .map_err(|error| format!("Couldn't make {}: {error}.", unpacked.display()))?;
    let mut spec = probe_spec(&tar_program());
    spec.args = vec![
        "-xf".into(),
        dir.join(&name).into_os_string(),
        "-C".into(),
        unpacked.clone().into_os_string(),
    ];
    let ran = run_spec(launcher, &spec, b"", ARCHIVE_TIMEOUT)
        .await
        .map_err(|error| format!("Couldn't unpack {name}: {error}."))?;
    if ran.exit_code != Some(0) {
        return Err(format!(
            "Couldn't unpack {name}: {}",
            last_line(&ran.stderr_tail)
        ));
    }
    let root = gh_root(&unpacked).ok_or_else(|| format!("{name} has no bin/gh in it."))?;
    let target = tools.join("gh");
    // A leftover that no longer resolves, since an installed gh would have refused the install.
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|error| format!("Couldn't remove {}: {error}.", target.display()))?;
    }
    std::fs::rename(&root, &target)
        .map_err(|error| format!("Couldn't move gh into {}: {error}.", target.display()))
}

/// Downloads the release `release_url` describes into `dir`: its archive for this platform, under
/// its own name, which this returns, and its checksums as `checksums.txt`.
async fn download_release(
    launcher: &Launcher,
    release_url: &str,
    dir: &Path,
) -> Result<String, String> {
    let release_file = dir.join("release.json");
    download(
        launcher,
        "the latest gh release",
        release_url,
        &release_file,
        SMALL_DOWNLOAD_TIMEOUT,
    )
    .await?;
    let release: Release = std::fs::read(&release_file)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .ok_or("GitHub described the latest gh release in a way plxd can't read.")?;
    let version = release.tag_name.trim_start_matches('v');
    let asset = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| format!("gh {version}'s release has no {name}."))
    };
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let name = asset_name(version, os, arch)
        .ok_or_else(|| format!("gh has no build for {os} on {arch}."))?;
    let checksums = asset(&format!("gh_{version}_checksums.txt"))?;
    download(
        launcher,
        "gh's checksums",
        &checksums.browser_download_url,
        &dir.join("checksums.txt"),
        SMALL_DOWNLOAD_TIMEOUT,
    )
    .await?;
    download(
        launcher,
        &name,
        &asset(&name)?.browser_download_url,
        &dir.join(&name),
        ARCHIVE_TIMEOUT,
    )
    .await?;
    Ok(name)
}

/// Downloads `url` to `file` with the system `curl`, ignoring any `.curlrc` (`-q`).
async fn download(
    launcher: &Launcher,
    what: &str,
    url: &str,
    file: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let mut spec = probe_spec("curl");
    spec.args = vec![
        "-q".into(),
        "--fail".into(),
        "--silent".into(),
        "--show-error".into(),
        "--location".into(),
        "--proto-redir".into(),
        "=https".into(),
        "--output".into(),
        file.as_os_str().to_owned(),
        url.into(),
    ];
    let ran = run_spec(launcher, &spec, b"", timeout)
        .await
        .map_err(|error| format!("Couldn't download {what}: {error}."))?;
    if ran.exit_code == Some(0) {
        Ok(())
    } else {
        Err(format!(
            "Couldn't download {what}: {}",
            last_line(&ran.stderr_tail)
        ))
    }
}

/// The folder in `unpacked` that holds `bin/gh`: itself, as in the Windows zip, or the one folder
/// the macOS and Linux archives put everything in.
fn gh_root(unpacked: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "gh.exe" } else { "gh" };
    let has_gh = |dir: &Path| dir.join("bin").join(exe).is_file();
    if has_gh(unpacked) {
        return Some(unpacked.to_owned());
    }
    std::fs::read_dir(unpacked)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| has_gh(dir))
}

/// The `tar` that unpacks gh's archive. macOS and Windows get a zip, which only the system's
/// bsdtar reads, so they skip a GNU tar earlier on `PATH` (nix's, or Git for Windows').
fn tar_program() -> String {
    if cfg!(target_os = "macos") {
        "/usr/bin/tar".to_owned()
    } else if cfg!(windows)
        && let Ok(root) = std::env::var("SystemRoot")
    {
        format!(r"{root}\System32\tar.exe")
    } else {
        "tar".to_owned()
    }
}

#[cfg(all(test, unix))]
mod tests;
