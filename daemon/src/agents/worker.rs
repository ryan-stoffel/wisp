//! What a worker needs before it starts (0013, decision 0014): the environment agents run in,
//! the checks that refuse a worker wispd can't sandbox, the key accounts routing reads, and the
//! prompt that tells the agent its limits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{AccountId, CliKind, DetectedCli, ErrorKind, Provider};

use crate::backend::Backend;
use crate::backend::claude::{self, WORKER_MIN_VERSION, parse_version};
use crate::backend::process::Environment;
use crate::routing::KeyAccounts;

/// Folders appended to an agent's `PATH` when it lacks them (#96): the vendors' own install
/// folder (`~/.local/bin`, where Claude Code's installer puts `claude`), Homebrew on Apple silicon
/// and on Intel, and the system folders. They go after whatever `PATH` wispd was started with, so
/// the user's own order still wins; they only fill in what launchd or an SSH session left out.
const EXTRA_PATH_IN_HOME: &[&str] = &[".local/bin"];
const EXTRA_PATH: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// The variables of wispd's own environment that agent CLIs, CLI probes, and worktree git
/// commands inherit; everything else stays with wispd (0013, decision 0014). A worker has network
/// access and reads prompt-injectable content, so a token wispd happened to start with, such as
/// `GITHUB_TOKEN`, `NPM_TOKEN`, `OPENAI_API_KEY`, or `AWS_SECRET_ACCESS_KEY`, must never reach
/// it. These are what a CLI needs to find itself, its home folder, its temp folder, the user's
/// locale and terminal, and a corporate network's proxy and certificates. A backend adds its own
/// variables on top, such as an account's API key or configuration folder, and the launcher
/// adds `WISPD_DATA_DIR`.
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

/// The environment every agent CLI, CLI probe, and worktree git command starts from (#96,
/// decision 0014): only the [`INHERITED`] part of wispd's own, with [`EXTRA_PATH`] filled in.
pub(crate) fn agent_environment() -> Environment {
    with_extra_path(
        allowlisted(&Environment::inherited()),
        std::env::home_dir().as_deref(),
    )
}

/// The variables of `env` an agent may inherit: [`INHERITED`] and [`INHERITED_PREFIXES`].
pub(crate) fn allowlisted(env: &Environment) -> Environment {
    env.names()
        .filter(|name| {
            name.to_str().is_some_and(|name| {
                INHERITED.contains(&name)
                    || INHERITED_PREFIXES
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
            })
        })
        .filter_map(|name| Some((name.to_owned(), env.get(name)?.to_owned())))
        .collect()
}

pub(crate) fn with_extra_path(mut env: Environment, home: Option<&Path>) -> Environment {
    let mut dirs: Vec<PathBuf> = env
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let in_home = home
        .filter(|home| home.is_absolute())
        .into_iter()
        .flat_map(|home| EXTRA_PATH_IN_HOME.iter().map(move |dir| home.join(dir)));
    for dir in in_home.chain(EXTRA_PATH.iter().map(PathBuf::from)) {
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
    ErrorObject::wisp(ErrorKind::WorkerUnavailable, message)
}

/// Refuses a worker on a backend that doesn't enforce the worker sandbox (0013).
pub(super) fn check_backend(backend: &dyn Backend) -> Result<(), ErrorObject> {
    if backend.capabilities().worker_sandbox {
        Ok(())
    } else {
        Err(worker_unavailable(format!(
            "the {} backend can't run a sandboxed worker yet (decision 0013); choose a Claude \
             Code account",
            backend.name()
        )))
    }
}

/// Refuses a Claude worker unless the detected Claude Code is at least [`WORKER_MIN_VERSION`],
/// which has every flag the worker sandbox needs (0013).
pub(super) fn check_claude(detected: Option<&DetectedCli>) -> Result<(), ErrorObject> {
    let Some(detected) = detected.filter(|detected| detected.installed) else {
        return Err(worker_unavailable(format!(
            "Claude Code isn't installed on this host; install Claude Code {WORKER_MIN_VERSION} \
             or later"
        )));
    };
    let Some(version) = detected.version.as_deref() else {
        return Err(worker_unavailable(format!(
            "wispd could not read Claude Code's version, and a sandboxed worker needs \
             {WORKER_MIN_VERSION} or later; update Claude Code"
        )));
    };
    match parse_version(version) {
        Some(found) if Some(found) >= parse_version(WORKER_MIN_VERSION) => Ok(()),
        _ => Err(worker_unavailable(format!(
            "Claude Code {version} can't run a sandboxed worker; update Claude Code to \
             {WORKER_MIN_VERSION} or later"
        ))),
    }
}

/// The detected CLI a backend runs, if wispd checks its version before starting a worker.
pub(super) fn cli_of(backend: &dyn Backend) -> Option<CliKind> {
    (backend.name() == claude::PROGRAM).then_some(CliKind::Claude)
}

/// Characters the vendors read as wildcards in a sandbox path (0013).
const GLOB_CHARACTERS: &[char] = &['*', '?', '[', ']'];

/// `path`, canonical, and refused with a plain message if it isn't UTF-8 or holds a wildcard.
/// Seatbelt matches real paths, and `/tmp` and `/var` are symlinks on macOS (0013).
pub(super) fn sandbox_path(path: &Path, what: &str) -> Result<PathBuf, ErrorObject> {
    let canonical = path.canonicalize().map_err(|error| {
        worker_unavailable(format!(
            "could not resolve {what} {}: {error}",
            path.display()
        ))
    })?;
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
        "You are a wisp worker agent in a git worktree at {worktree}.\n\
         - You may write files only in that worktree and in the project's shared context folder \
         at {context}. Put notes there that the user or other agents should see.\n\
         - Your commands have network access, but this Mac's own services (localhost) are \
         unreachable.\n\
         - Don't commit or change git history: wisp commits your changes when you finish.\n\
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
    use std::path::Path;

    use wisp_protocol::{CliKind, DetectedCli, ErrorKind};

    use super::{allowlisted, check_claude, sandbox_path, with_extra_path};
    use crate::backend::process::{ALWAYS_SCRUBBED, Environment};

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
    fn an_old_or_unknown_claude_is_refused_naming_both_versions() {
        assert!(check_claude(Some(&claude(Some("2.1.248")))).is_ok());
        assert!(check_claude(Some(&claude(Some("2.2.0")))).is_ok());
        let old = check_claude(Some(&claude(Some("2.1.247")))).unwrap_err();
        assert_eq!(old.wisp_data().unwrap().kind, ErrorKind::WorkerUnavailable);
        assert!(old.message.contains("2.1.247"), "{}", old.message);
        assert!(old.message.contains("2.1.248"), "{}", old.message);
        for missing in [None, Some(&claude(None)), Some(&claude(Some("latest")))] {
            let error = check_claude(missing).unwrap_err();
            assert!(error.message.contains("2.1.248"), "{}", error.message);
        }
        let mut absent = claude(None);
        absent.installed = false;
        assert!(
            check_claude(Some(&absent))
                .unwrap_err()
                .message
                .contains("isn't installed")
        );
    }

    #[test]
    fn a_minimal_path_is_filled_in_after_the_users_own_folders() {
        let mut env = Environment::empty();
        env.set("PATH", "/usr/bin:/custom/bin");
        let env = with_extra_path(env, Some(Path::new("/Users/me")));
        assert_eq!(
            path_entries(&env),
            [
                "/usr/bin",
                "/custom/bin",
                "/Users/me/.local/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
            ]
        );
        let unset = with_extra_path(Environment::empty(), None);
        assert_eq!(path_entries(&unset)[0], "/opt/homebrew/bin");
    }

    /// wispd started from a shell that holds credentials for other services: a worker spawned
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
        let mut wispd_env = Environment::empty();
        wispd_env.set("PATH", "/usr/bin:/bin");
        for name in secrets {
            wispd_env.set(name, "secret-value");
        }
        for (name, value) in kept {
            wispd_env.set(name, value);
        }
        let dir = tempfile::tempdir().unwrap();
        let launcher = Launcher::new(
            DataDir::new(dir.path()).unwrap(),
            with_extra_path(allowlisted(&wispd_env), None),
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
                policy: ToolPolicy::NoWrite,
                sandbox: None,
                account: AccountRef {
                    id: "fake".into(),
                    credential: Credential::Subscription { config_home: None },
                },
                resume: None,
                model: None,
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

    #[test]
    fn a_path_with_a_wildcard_is_refused_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("app[old]");
        std::fs::create_dir(&odd).unwrap();
        let error = sandbox_path(&odd, "the repository").unwrap_err();
        assert_eq!(
            error.wisp_data().unwrap().kind,
            ErrorKind::WorkerUnavailable
        );
        assert!(error.message.contains("app[old]"), "{}", error.message);
        let fine = sandbox_path(dir.path(), "the repository").unwrap();
        assert!(fine.is_absolute());
        assert!(sandbox_path(&dir.path().join("missing"), "x").is_err());
    }
}
