//! What a worker needs before it starts (0013, decision 0014): the environment agents run in,
//! the checks that refuse a worker plxd can't sandbox, the key accounts routing reads, and the
//! prompt that tells the agent its limits.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{AccountId, CliKind, DetectedCli, ErrorKind, Provider};

use crate::backend::Backend;
use crate::backend::claude::{self, WORKER_MIN_VERSION, parse_version};
use crate::backend::codex;
use crate::backend::process::Environment;
use crate::paths::without_verbatim_prefix;
use crate::routing::KeyAccounts;

/// Folders appended to an agent's `PATH` when it lacks them (#96): the vendors' own install
/// folder (`~/.local/bin`, where Claude Code's installer puts `claude`; `%USERPROFILE%\.local\bin`
/// on Windows), rustup's `~/.cargo/bin` (RYA-126), Homebrew on Apple silicon (macOS only) and
/// `/usr/local/bin`, and the system folders (0023). They go after whatever `PATH` plxd was
/// started with, so the user's own order still wins; they only fill in what launchd or an SSH
/// session left out.
const EXTRA_PATH_IN_HOME: &[&str] = &[".local/bin", ".cargo/bin"];
#[cfg(target_os = "macos")]
const EXTRA_PATH: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];
#[cfg(target_os = "linux")]
const EXTRA_PATH: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];
#[cfg(windows)]
const EXTRA_PATH: &[&str] = &[];

/// The variables of plxd's own environment that agent CLIs, CLI probes, and worktree git
/// commands inherit; everything else stays with plxd (0013, decision 0014). A worker has network
/// access and reads prompt-injectable content, so a token plxd happened to start with, such as
/// `GITHUB_TOKEN`, `NPM_TOKEN`, `OPENAI_API_KEY`, or `AWS_SECRET_ACCESS_KEY`, must never reach
/// it. These are what a CLI needs to find itself, its home folder, its temp folder, the user's
/// locale and terminal, and a corporate network's proxy and certificates. A backend adds its own
/// variables on top, such as an account's API key or configuration folder, and the launcher
/// adds `PLXD_DATA_DIR`.
const INHERITED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "TERM",
    "__CF_USER_TEXT_ENCODING",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
];

/// Prefixes of inherited variables that are kept too: the locale categories.
const INHERITED_PREFIXES: &[&str] = &["LC_"];

/// What Windows programs need on top of [`INHERITED`] to find the system, the user's folders,
/// and themselves: without `SYSTEMROOT`, for one, Node can't reach the network. Names are
/// upper-case, as [`Environment::inherited`] spells them there.
const INHERITED_ON_WINDOWS: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "TEMP",
    "TMP",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
];

/// The environment every agent CLI, CLI probe, and worktree git command starts from (#96,
/// decision 0014): only the [`INHERITED`] part of plxd's own, with the login shell's `PATH`
/// ([`login_shell_path`]) and [`EXTRA_PATH`] filled in.
pub(crate) fn agent_environment() -> Environment {
    #[cfg(unix)]
    let login = std::env::var_os("SHELL").and_then(|shell| login_shell_path(&shell));
    #[cfg(not(unix))]
    let login: Option<OsString> = None;
    with_extra_path(
        allowlisted(&Environment::inherited()),
        std::env::home_dir().as_deref(),
        login.as_deref(),
    )
}

/// How long [`login_shell_path`] waits for the user's shell.
#[cfg(unix)]
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Marks the `PATH` in the login shell's output, which its startup files may print around.
#[cfg(unix)]
const PATH_MARK: &str = "__PLXD_PATH__";

/// The `PATH` that `shell`, run as the user's login, interactive shell, sets up (PLX-323). An app
/// opened from the Dock, and the plxd it starts, get launchd's `/usr/bin:/bin:/usr/sbin:/sbin`,
/// which misses what the user's startup files add, such as nix-darwin's
/// `/run/current-system/sw/bin` or nvm's node. The shell runs detached in its own session, so it
/// can't take over a terminal. `None` if it fails, prints no `PATH`, or takes longer than
/// [`LOGIN_SHELL_TIMEOUT`], in which case its process group is killed.
#[cfg(unix)]
pub(crate) fn login_shell_path(shell: &OsStr) -> Option<OsString> {
    use std::io::Read as _;
    use std::os::fd::AsFd as _;

    use rustix::process::{Signal, WaitOptions, kill_process_group, waitpid};

    let mut command = std::process::Command::new(shell);
    command.args([
        "-l",
        "-i",
        "-c",
        &format!("printf '%s%s%s' {PATH_MARK} \"$PATH\" {PATH_MARK}"),
    ]);
    let null = std::fs::File::open("/dev/null").ok()?;
    let (mut reader, writer) = std::io::pipe().ok()?;
    let stdio = crate::spawn::Stdio {
        stdin: null.as_fd(),
        stdout: writer.as_fd(),
        stderr: null.as_fd(),
    };
    let pid = crate::spawn::spawn_detached(&command, stdio).ok()?;
    drop(writer);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        let _ = sender.send(output);
    });
    let output = receiver.recv_timeout(LOGIN_SHELL_TIMEOUT);
    if output.is_err() {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    let _ = waitpid(Some(pid), WaitOptions::empty());
    let output = String::from_utf8(output.ok()?).ok()?;
    let (_, rest) = output.split_once(PATH_MARK)?;
    let (path, _) = rest.split_once(PATH_MARK)?;
    (!path.is_empty()).then(|| path.into())
}

/// The variables of `env` an agent may inherit: [`INHERITED`] and [`INHERITED_PREFIXES`], and on
/// Windows [`INHERITED_ON_WINDOWS`].
pub(crate) fn allowlisted(env: &Environment) -> Environment {
    env.names()
        .filter(|name| {
            name.to_str().is_some_and(|name| {
                INHERITED.contains(&name)
                    || (cfg!(windows) && INHERITED_ON_WINDOWS.contains(&name))
                    || INHERITED_PREFIXES
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
            })
        })
        .filter_map(|name| Some((name.to_owned(), env.get(name)?.to_owned())))
        .collect()
}

/// `env` with the folders of `login`, the login shell's `PATH`, then [`EXTRA_PATH_IN_HOME`] and
/// [`EXTRA_PATH`], appended to its `PATH` where missing.
pub(crate) fn with_extra_path(
    mut env: Environment,
    home: Option<&Path>,
    login: Option<&OsStr>,
) -> Environment {
    let mut dirs: Vec<PathBuf> = env
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    // An rc file's empty or relative entry would resolve in the agent's worktree.
    let login = login
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute());
    let in_home = home
        .filter(|home| home.is_absolute())
        .into_iter()
        .flat_map(|home| EXTRA_PATH_IN_HOME.iter().map(move |dir| home.join(dir)));
    for dir in login
        .chain(in_home)
        .chain(EXTRA_PATH.iter().map(PathBuf::from))
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Ok(path) = std::env::join_paths(dirs) {
        env.set("PATH", path);
    }
    env
}

pub(super) fn worker_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject::parallax(ErrorKind::WorkerUnavailable, message)
}

/// What to do about a backend that can't sandbox a worker here: Claude Code's sandbox is checked
/// on macOS and Linux (0013).
#[cfg(not(windows))]
const NO_SANDBOX_HINT: &str = "choose a Claude Code account";

/// What to do about a backend that can't sandbox a worker here: Claude Code has no sandbox on
/// native Windows (0023, RYA-24).
#[cfg(windows)]
const NO_SANDBOX_HINT: &str = "Claude Code has no sandbox on native Windows, so run plxd in \
                               WSL2 and add that as the host for workers";

/// Refuses a worker on a backend that doesn't enforce the worker sandbox (0013).
pub(super) fn check_backend(backend: &dyn Backend) -> Result<(), ErrorObject> {
    if backend.capabilities().worker_sandbox {
        return Ok(());
    }
    let why = if backend.name() == codex::PROGRAM {
        "Codex workers are turned off until RYA-145 keeps their commands out of the shared temp \
         folders"
            .to_owned()
    } else {
        format!(
            "the {} backend can't run a sandboxed worker yet (decision 0013)",
            backend.name()
        )
    };
    Err(worker_unavailable(format!("{why}; {NO_SANDBOX_HINT}")))
}

/// Refuses a worker unless the detected `cli` is at least its backend's oldest version with
/// everything the worker sandbox needs (0013): Claude Code's [`WORKER_MIN_VERSION`], or Codex's
/// [`codex::WORKER_MIN_VERSION`], below which Codex would ignore the sandbox's settings.
pub(super) fn check_version(
    cli: CliKind,
    detected: Option<&DetectedCli>,
) -> Result<(), ErrorObject> {
    let (name, min) = match cli {
        CliKind::Codex => ("Codex", codex::WORKER_MIN_VERSION),
        _ => ("Claude Code", WORKER_MIN_VERSION),
    };
    let Some(detected) = detected.filter(|detected| detected.installed) else {
        return Err(worker_unavailable(format!(
            "{name} isn't installed on this host; install {name} {min} or later"
        )));
    };
    let Some(version) = detected.version.as_deref() else {
        return Err(worker_unavailable(format!(
            "plxd could not read {name}'s version, and a sandboxed worker needs {min} or later; \
             update {name}"
        )));
    };
    match parse_version(version) {
        Some(found) if Some(found) >= parse_version(min) => Ok(()),
        _ => Err(worker_unavailable(format!(
            "{name} {version} can't run a sandboxed worker; update {name} to {min} or later"
        ))),
    }
}

/// Refuses a Claude worker unless Claude Code's sandbox works on this Linux host, seccomp filter
/// included (0013), for the `claude` that `check_version` accepted.
#[cfg(target_os = "linux")]
pub(super) async fn check_linux_sandbox(
    detector: &crate::detect::CliDetector,
    claude: Option<&DetectedCli>,
) -> Result<(), ErrorObject> {
    let Some(path) = claude.and_then(|claude| claude.path.as_deref()) else {
        return Err(worker_unavailable(
            "plxd could not tell where Claude Code is installed",
        ));
    };
    claude::linux_sandbox::check_host(detector.launcher(), Path::new(path))
        .await
        .map_err(worker_unavailable)
}

/// The detected CLI a backend runs, if plxd checks its version before starting a worker.
pub(super) fn cli_of(backend: &dyn Backend) -> Option<CliKind> {
    match backend.name() {
        claude::PROGRAM => Some(CliKind::Claude),
        codex::PROGRAM => Some(CliKind::Codex),
        _ => None,
    }
}

/// Characters the vendors read as wildcards in a sandbox path (0013).
const GLOB_CHARACTERS: &[char] = &['*', '?', '[', ']'];

/// `path`, canonical, and refused with a plain message if it isn't UTF-8 or holds a wildcard.
/// Seatbelt matches real paths, and `/tmp` and `/var` are symlinks on macOS (0013). On Windows it
/// is spelled without the verbatim `\\?\` prefix canonicalizing adds, whose `?` isn't part of the
/// path (RYA-109), and refused if only a verbatim path can name it.
pub(super) fn sandbox_path(path: &Path, what: &str) -> Result<PathBuf, ErrorObject> {
    let canonical = path.canonicalize().map_err(|error| {
        worker_unavailable(format!(
            "could not resolve {what} {}: {error}",
            path.display()
        ))
    })?;
    let Some(canonical) = without_verbatim_prefix(&canonical) else {
        return Err(worker_unavailable(format!(
            "{what} {} can only be named with a \\\\?\\ path, which the worker sandbox can't \
             hold; move it to a folder on a drive letter or network share whose folder names \
             don't end in a dot or a space and aren't reserved names such as CON or NUL",
            canonical.display()
        )));
    };
    match canonical.to_str() {
        Some(text) if !text.contains(GLOB_CHARACTERS) => Ok(canonical),
        _ => Err(worker_unavailable(format!(
            "{what} {} has a *, ?, [, or ] in its path, which the worker sandbox can't hold \
             (decision 0013); move it to a path without them",
            canonical.display()
        ))),
    }
}

/// Key accounts as routing (#119) reads them: a snapshot of the `accounts` table.
pub(super) struct StoredKeyAccounts(pub HashMap<AccountId, Provider>);

impl KeyAccounts for StoredKeyAccounts {
    fn provider_of(&self, id: AccountId) -> Option<Provider> {
        self.0.get(&id).copied()
    }

    fn fallback_for(&self, provider: Provider) -> Option<AccountId> {
        self.0
            .iter()
            .filter(|(_, candidate)| **candidate == provider)
            .map(|(id, _)| *id)
            .min()
    }
}

/// The first message of a new worker: its limits (0013), then the task.
pub(super) fn worker_prompt(task: &str, worktree: &Path, context: &Path) -> String {
    format!(
        "You are a Parallax worker agent in a git worktree at {worktree}.\n\
         - You may write files only in that worktree and in the project's shared context folder \
         at {context}. Put notes there that the user or other agents should see.\n\
         - Your commands have network access, but this Mac's own services (localhost) are \
         unreachable.\n\
         - Don't commit or change git history: Parallax commits your changes when you finish.\n\
         - The project's dependencies may not be installed.\n\
         \n\
         Your task:\n{task}",
        worktree = worktree.display(),
        context = context.display(),
    )
}

/// The home folder, for the sandbox's list of unreadable paths.
pub(super) fn home() -> Result<PathBuf, ErrorObject> {
    let home = std::env::home_dir()
        .filter(|home| home.is_absolute())
        .ok_or_else(|| worker_unavailable("the home folder is unknown"))?;
    sandbox_path(&home, "the home folder")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

    use parallax_protocol::{CliKind, DetectedCli, ErrorKind};

    #[cfg(unix)]
    use std::ffi::OsStr;

    #[cfg(unix)]
    use super::{allowlisted, login_shell_path, with_extra_path};
    use super::{check_version, sandbox_path};
    use crate::backend::process::ALWAYS_SCRUBBED;
    #[cfg(unix)]
    use crate::backend::process::Environment;

    #[cfg(unix)]
    fn path_entries(env: &Environment) -> Vec<String> {
        env.get("PATH")
            .map(|path| {
                std::env::split_paths(path)
                    .map(|entry| entry.into_os_string().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn claude(version: Option<&str>) -> DetectedCli {
        DetectedCli {
            cli: CliKind::Claude,
            installed: true,
            path: Some("/usr/local/bin/claude".into()),
            version: version.map(str::to_owned),
            signed_in: Some(true),
            auth_kind: None,
            plan: None,
            note: None,
        }
    }

    #[test]
    fn an_old_or_unknown_cli_is_refused_naming_both_versions() {
        let check = |version| check_version(CliKind::Claude, Some(&claude(version)));
        assert!(check(Some("2.1.248")).is_ok());
        assert!(check(Some("2.2.0")).is_ok());
        let old = check(Some("2.1.247")).unwrap_err();
        assert_eq!(
            old.parallax_data().unwrap().kind,
            ErrorKind::WorkerUnavailable
        );
        assert!(old.message.contains("2.1.247"), "{}", old.message);
        assert!(old.message.contains("2.1.248"), "{}", old.message);
        for missing in [None, Some(&claude(None)), Some(&claude(Some("latest")))] {
            let error = check_version(CliKind::Claude, missing).unwrap_err();
            assert!(error.message.contains("2.1.248"), "{}", error.message);
        }
        let mut absent = claude(None);
        absent.installed = false;
        assert!(
            check_version(CliKind::Claude, Some(&absent))
                .unwrap_err()
                .message
                .contains("isn't installed")
        );

        let mut codex = claude(Some("0.157.1"));
        codex.cli = CliKind::Codex;
        assert!(check_version(CliKind::Codex, Some(&codex)).is_ok());
        codex.version = Some("0.156.1".into());
        let old = check_version(CliKind::Codex, Some(&codex)).unwrap_err();
        assert!(old.message.contains("Codex 0.156.1"), "{}", old.message);
        assert!(old.message.contains("0.157.1"), "{}", old.message);
    }

    #[cfg(unix)]
    #[test]
    fn a_minimal_path_is_filled_in_after_the_users_own_folders() {
        let mut env = Environment::empty();
        env.set("PATH", "/usr/bin:/custom/bin");
        let login = OsStr::new("/run/current-system/sw/bin::.:/usr/bin");
        let env = with_extra_path(env, Some(Path::new("/Users/me")), Some(login));
        let mut expected = vec![
            "/usr/bin",
            "/custom/bin",
            "/run/current-system/sw/bin",
            "/Users/me/.local/bin",
            "/Users/me/.cargo/bin",
        ];
        if cfg!(target_os = "macos") {
            expected.push("/opt/homebrew/bin");
        }
        expected.extend(["/usr/local/bin", "/bin", "/usr/sbin", "/sbin"]);
        assert_eq!(path_entries(&env), expected);
        let unset = with_extra_path(Environment::empty(), None, None);
        assert_eq!(path_entries(&unset)[0], super::EXTRA_PATH[0]);
    }

    #[cfg(unix)]
    #[test]
    fn the_login_shells_path_is_read_past_what_its_startup_files_print() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        // A stand-in for zsh: its "startup files" print a banner and add a folder to PATH, then
        // it runs the -c command it was given, after -l and -i.
        std::fs::write(
            &shell,
            "#!/bin/sh\necho 'Welcome back'\nPATH=/from/login:$PATH\nexec /bin/sh -c \"$4\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = login_shell_path(shell.as_os_str()).unwrap();
        assert!(
            path.to_str().unwrap().starts_with("/from/login:"),
            "{path:?}"
        );
        assert_eq!(login_shell_path(OsStr::new("/nonexistent/shell")), None);
    }

    #[cfg(unix)]
    /// plxd started from a shell that holds credentials for other services: a worker spawned
    /// from the agent environment sees none of them, but keeps what a CLI needs.
    #[tokio::test]
    async fn a_spawned_worker_inherits_only_the_allowlist() {
        use crate::backend::fake::{FakeBackend, Script, Step};
        use crate::backend::process::Launcher;
        use crate::backend::{
            AccountRef, Backend, Credential, Event, RunId, RunRequest, ToolPolicy,
        };
        use crate::paths::DataDir;

        let secrets = [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "NPM_TOKEN",
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SESSION_TOKEN",
            "DATABASE_URL",
        ];
        let kept = [
            ("HOME", "/Users/me"),
            ("LANG", "en_US.UTF-8"),
            ("LC_CTYPE", "UTF-8"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("NODE_EXTRA_CA_CERTS", "/etc/corp.pem"),
        ];
        let mut plxd_env = Environment::empty();
        plxd_env.set("PATH", "/usr/bin:/bin");
        for name in secrets {
            plxd_env.set(name, "secret-value");
        }
        for (name, value) in kept {
            plxd_env.set(name, value);
        }
        let dir = tempfile::tempdir().unwrap();
        let launcher = Launcher::new(
            DataDir::new(dir.path()).unwrap(),
            with_extra_path(allowlisted(&plxd_env), None, None),
        );
        let names: Vec<&str> = secrets
            .iter()
            .chain(kept.iter().map(|(n, _)| n))
            .copied()
            .collect();
        let script = Script {
            steps: names
                .iter()
                .map(|name| Step::EchoEnv((*name).to_owned()))
                .collect(),
        };
        let backend = FakeBackend::new(launcher, script);
        let mut started = backend
            .start(RunRequest {
                run_id: RunId::generate(),
                turn_id: None,
                cwd: dir.path().canonicalize().unwrap(),
                prompt: "print the environment".into(),
                images: Vec::new(),
                policy: ToolPolicy::NoWrite,
                sandbox: None,
                account: AccountRef {
                    id: "fake".into(),
                    credential: Credential::Subscription { config_home: None },
                },
                resume: None,
                model: None,
                effort: None,
                permission: None,
                context_window: None,
                fast: None,
                coordinator_tools: None,
                thread_tools: None,
                approvals: false,
                thread: false,
            })
            .unwrap();
        let mut seen = Vec::new();
        while let Some(event) = started.events.next().await {
            if let Event::Text { text, .. } = event {
                seen.push(text);
            }
        }
        let expected: Vec<String> = secrets
            .iter()
            .map(|_| "<unset>".to_owned())
            .chain(kept.iter().map(|(_, value)| (*value).to_owned()))
            .collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn the_ssh_session_never_reaches_an_agent() {
        for name in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK"] {
            assert!(ALWAYS_SCRUBBED.contains(&name), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_with_a_wildcard_is_refused_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("app[old]");
        std::fs::create_dir(&odd).unwrap();
        let error = sandbox_path(&odd, "the repository").unwrap_err();
        assert_eq!(
            error.parallax_data().unwrap().kind,
            ErrorKind::WorkerUnavailable
        );
        assert!(error.message.contains("app[old]"), "{}", error.message);
        let fine = sandbox_path(dir.path(), "the repository").unwrap();
        assert!(fine.is_absolute());
        assert!(sandbox_path(&dir.path().join("missing"), "x").is_err());
    }

    /// Canonicalizing on Windows adds `\\?\`, whose `?` refused every path (RYA-109). The path
    /// comes back plain, and a wildcard in the path itself is still refused.
    #[cfg(windows)]
    #[test]
    fn a_windows_path_comes_back_without_its_verbatim_prefix() {
        use std::path::{Component, Prefix};

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("app");
        std::fs::create_dir(&repo).unwrap();
        for path in [repo.clone(), repo.join("..").join("app")] {
            let plain = sandbox_path(&path, "the repository").unwrap();
            assert!(
                matches!(
                    plain.components().next(),
                    Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
                ),
                "{}",
                plain.display()
            );
            assert!(
                !plain.to_str().unwrap().contains('?'),
                "{}",
                plain.display()
            );
            assert!(plain.ends_with("app"));
            assert_eq!(plain.canonicalize().unwrap(), repo.canonicalize().unwrap());
        }
        let home = super::home().unwrap();
        assert!(
            !home.to_str().unwrap().starts_with(r"\\?\"),
            "{}",
            home.display()
        );

        let odd = dir.path().join("app[old]");
        std::fs::create_dir(&odd).unwrap();
        let error = sandbox_path(&odd, "the repository").unwrap_err();
        assert!(error.message.contains("app[old]"), "{}", error.message);
        assert!(!error.message.contains(r"\\?\"), "{}", error.message);
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_only_a_verbatim_path_can_name_is_refused_plainly() {
        let dir = tempfile::tempdir().unwrap();
        // Only the verbatim spelling keeps the trailing dot.
        let verbatim = dir.path().canonicalize().unwrap().join("trailing.");
        std::fs::create_dir(&verbatim).unwrap();
        let error = sandbox_path(&verbatim, "the repository").unwrap_err();
        assert_eq!(
            error.parallax_data().unwrap().kind,
            ErrorKind::WorkerUnavailable
        );
        assert!(error.message.contains("trailing."), "{}", error.message);
        assert!(error.message.contains("CON"), "{}", error.message);
    }
}
