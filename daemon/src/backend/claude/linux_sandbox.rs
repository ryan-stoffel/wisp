//! Checks that a Linux host can run Claude Code's worker sandbox, seccomp filter included, before
//! a worker starts (0013's Linux section).
//!
//! On Linux, Claude Code sandboxes each command with bubblewrap, relays its network through
//! `socat`, and blocks Unix sockets with a seccomp filter. `failIfUnavailable` covers a missing
//! `bwrap` or `socat`, but not the filter, which Claude Code treats as optional. Without it a
//! command can reach any Unix socket, including the D-Bus session bus that serves the Secret
//! Service, and `docker.sock`. Since 2.1.92 the filter ships inside Claude Code: its native build
//! runs every sandboxed command through its own binary as `ARGV0=apply-seccomp`. So
//! [`check_host`] probes that same helper, inside the namespaces Claude's sandbox uses.
//!
//! [`check_host`] also refuses a Claude Code that runs with `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` on,
//! which widens every command's writes on Linux (RYA-112).

use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::Value;

use super::{WORKER_MIN_VERSION, scrubbed};
use crate::backend::process::{Launcher, ProcessSpec};
use crate::detect::{PROBE_TIMEOUT, Ran, resolve, run, run_spec};

/// The namespaces Claude Code's sandbox puts each command in (sandbox-runtime's bubblewrap
/// arguments): a new session, user, and PID namespace with no capabilities, and a fresh `/proc`.
/// The probe leaves out its network namespace and mount rules, which it doesn't need.
const BWRAP_ARGS: &[&str] = &[
    "--new-session",
    "--die-with-parent",
    "--unshare-user",
    "--unshare-pid",
    "--cap-drop",
    "ALL",
    "--dev-bind",
    "/",
    "/",
    "--proc",
    "/proc",
];

/// `socat` (`$1`) connects to the Unix socket `$2`, and the shell exits with [`CONNECTED`] or
/// [`REFUSED`], codes that neither bwrap nor the helper uses.
const CONNECT: &str = r#""$1" -u OPEN:/dev/null "UNIX-CONNECT:$2" 2>/dev/null && exit 98; exit 97"#;
const CONNECTED: i32 = 98;
const REFUSED: i32 = 97;

/// The switch that keeps unprivileged programs from creating user namespaces, which Ubuntu turns
/// on from 24.04.
const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";

/// What plxd runs to read Claude Code's sandbox posture: one JSON line. `--restricted` makes it
/// load the settings a worker loads, and no others.
const STATUS_ARGS: &[&str] = &["--restricted", "sandbox", "status"];

/// The only `statusVersion` plxd knows how to read.
const STATUS_VERSION: u64 = 3;

/// The `sandbox status` field that is `"unsupported"` on Linux exactly when
/// `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` is on: the flag turns off Bash auto-allow.
const SCRUB_FIELD: &str = "autoAllowBashIfSandboxedSource";

/// Every other value 2.1.283 gives [`SCRUB_FIELD`]: where the auto-allow setting comes from.
const SCRUB_OFF: &[&str] = &["default", "settings", "policy"];

/// The end of every refusal for a status plxd can't read.
const CANT_TELL: &str = "so plxd can't tell whether CLAUDE_CODE_SUBPROCESS_ENV_SCRUB is on, which \
                         would widen a worker's sandbox";

/// The first Claude Code whose `sandbox status` is [`STATUS_VERSION`], with [`SCRUB_FIELD`].
const STATUS_MIN_VERSION: &str = "2.1.275";

/// Checks that Claude Code's sandbox works here with its seccomp filter, for workers that run
/// `claude` through `launcher`:
///
/// 1. `bwrap` and `socat` resolve on the launcher's `PATH`, where Claude Code looks for them.
/// 2. `claude` doesn't run with `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` on ([`check_scrub_flag`]).
/// 3. Inside bwrap alone, `socat` connects to a Unix socket plxd listens on. If it can't, bwrap
///    can't sandbox here, as on Ubuntu 24.04 and later without a profile for it.
/// 4. Inside bwrap and `claude`'s own filter, the same connect is refused.
///
/// # Errors
///
/// What is missing or broken, and the fix, as a message for `workerUnavailable`.
pub async fn check_host(launcher: &Launcher, claude: &Path) -> Result<(), String> {
    let bwrap = resolve(launcher, "bwrap").ok_or(
        "bubblewrap (bwrap) isn't installed, and Claude Code's worker sandbox needs it on Linux; \
         install the bubblewrap package",
    )?;
    let socat = resolve(launcher, "socat").ok_or(
        "socat isn't installed, and Claude Code's worker sandbox needs it on Linux; install the \
         socat package",
    )?;
    check_scrub_flag(launcher, text(claude)?).await?;
    let dir = tempfile::tempdir()
        .map_err(|error| format!("could not make a folder to check the worker sandbox: {error}"))?;
    let socket = dir.path().join("probe.sock");
    let _listener = UnixListener::bind(&socket)
        .map_err(|error| format!("could not listen on a socket to check the sandbox: {error}"))?;
    let (bwrap, socat, socket, claude) =
        (text(&bwrap)?, text(&socat)?, text(&socket)?, text(claude)?);
    let connect = ["/bin/sh", "-c", CONNECT, "sh", socat, socket];

    let alone = sandboxed(launcher, bwrap, &[], &connect).await?;
    match alone.exit_code {
        Some(CONNECTED) => {}
        Some(REFUSED) => {
            return Err(
                "socat can't connect to a Unix socket even outside Claude Code's seccomp filter, \
                 so plxd can't check the filter"
                    .into(),
            );
        }
        _ => return Err(bwrap_problem(&alone)),
    }

    let helper = [&[claude][..], &connect].concat();
    let filtered = sandboxed(
        launcher,
        bwrap,
        &["--setenv", "ARGV0", "apply-seccomp"],
        &helper,
    )
    .await?;
    match filtered.exit_code {
        Some(REFUSED) => Ok(()),
        Some(CONNECTED) => Err(format!(
            "Claude Code's seccomp filter let a sandboxed command connect to a Unix socket, which \
             would open the D-Bus session bus and docker.sock to workers; update Claude Code to \
             {WORKER_MIN_VERSION} or later"
        )),
        _ => Err(format!(
            "Claude Code's seccomp filter (its apply-seccomp helper) could not run: {}; Claude \
             Code {WORKER_MIN_VERSION} or later has it built in on x86_64 and arm64",
            first_line(&filtered)
        )),
    }
}

/// Refuses a `claude` that runs with `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` on. On Linux the flag
/// merges Claude Code's CI profile into every command's sandbox, which lets commands write all of
/// `/home`, `/tmp`, `/var`, `/opt`, `/run`, `/mnt`, and `/root` (0013). plxd never sets it for a
/// worker and drops it from what a worker inherits, but managed settings can set it, and their
/// `env` beats both the worker's environment and its `--settings`. So plxd asks Claude Code
/// itself, with the environment a worker gets. That was tested with the flag in
/// `managed-settings.json` and in a `managed-settings.d` drop-in. Server-managed settings, a
/// `policyHelper`, and WSL's inherited settings join the same managed tier inside Claude Code,
/// but weren't tried. This runs in its own process, which can still see other settings than the
/// worker does, so a worker whose `system/init` shows the permission mode the flag forces fails
/// too (RYA-118).
///
/// Fails closed: anything but a successful run that prints [`STATUS_VERSION`] with one of
/// [`SCRUB_OFF`] is refused.
async fn check_scrub_flag(launcher: &Launcher, claude: &str) -> Result<(), String> {
    read_status(launcher, &status_spec(launcher, claude)).await
}

/// `claude sandbox status`, with the environment scrubbed as a worker's is ([`scrubbed`]).
fn status_spec(launcher: &Launcher, claude: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new(claude, "/");
    spec.args = STATUS_ARGS.iter().map(Into::into).collect();
    spec.scrub = scrubbed(launcher.base());
    spec
}

/// Runs `spec`, a [`status_spec`], and reads its output as [`check_scrub_flag`] describes.
async fn read_status(launcher: &Launcher, spec: &ProcessSpec) -> Result<(), String> {
    let ran = run_spec(launcher, spec, b"", PROBE_TIMEOUT)
        .await
        .map_err(|error| format!("`claude sandbox status` failed ({error}), {CANT_TELL}"))?;
    if ran.exit_code != Some(0) {
        return Err(format!(
            "`claude sandbox status` failed ({}), {CANT_TELL}",
            first_line(&ran)
        ));
    }
    let status: Value = serde_json::from_str(ran.stdout.trim()).map_err(|error| {
        format!("`claude sandbox status` printed something plxd can't read ({error}), {CANT_TELL}")
    })?;
    match status["statusVersion"].as_u64() {
        Some(STATUS_VERSION) => {}
        Some(version) if version < STATUS_VERSION => {
            return Err(format!(
                "`claude sandbox status` has statusVersion {version}, {CANT_TELL}; update Claude \
                 Code to {STATUS_MIN_VERSION} or later"
            ));
        }
        _ => {
            return Err(format!(
                "`claude sandbox status` has statusVersion {}, and plxd reads only version \
                 {STATUS_VERSION}, {CANT_TELL}",
                status["statusVersion"]
            ));
        }
    }
    match status[SCRUB_FIELD].as_str() {
        Some(source) if SCRUB_OFF.contains(&source) => Ok(()),
        Some("unsupported") => Err(
            "Claude Code runs with CLAUDE_CODE_SUBPROCESS_ENV_SCRUB on, which on Linux lets a \
             worker's commands write all of /home, /tmp, /var, /opt, /run, /mnt, and /root. plxd \
             doesn't pass it on, so something Claude Code loads sets it, most likely the env block \
             of its managed settings (such as /etc/claude-code/managed-settings.json); turn it off \
             there"
                .into(),
        ),
        _ => Err(format!(
            "`claude sandbox status` reports {SCRUB_FIELD} as {}, which plxd doesn't know, \
             {CANT_TELL}",
            status[SCRUB_FIELD]
        )),
    }
}

/// Runs `command` in bwrap with [`BWRAP_ARGS`] and then `options`.
async fn sandboxed(
    launcher: &Launcher,
    bwrap: &str,
    options: &[&str],
    command: &[&str],
) -> Result<Ran, String> {
    let args = [BWRAP_ARGS, options, &["--"], command].concat();
    run(launcher, bwrap, &args, PROBE_TIMEOUT)
        .await
        .map_err(|error| format!("checking Claude Code's worker sandbox failed: {error}"))
}

/// Why bwrap couldn't run a command, from how it failed.
fn bwrap_problem(ran: &Ran) -> String {
    let apparmor = std::fs::read_to_string(APPARMOR_USERNS).is_ok_and(|value| value.trim() == "1");
    if apparmor {
        return "AppArmor keeps bubblewrap from creating the user namespaces Claude Code's worker \
                sandbox needs (kernel.apparmor_restrict_unprivileged_userns is 1); add an AppArmor \
                profile for bwrap, as Claude Code's sandboxing docs describe"
            .into();
    }
    format!(
        "bubblewrap can't create Claude Code's worker sandbox here: {}",
        first_line(ran)
    )
}

fn first_line(ran: &Ran) -> String {
    match ran
        .stderr_tail
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        Some(line) => line.to_owned(),
        None => match ran.exit_code {
            Some(code) => format!("it exited with code {code} and no message"),
            None => "it was killed by a signal".to_owned(),
        },
    }
}

fn text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} isn't valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::{check_host, read_status, status_spec};
    use crate::backend::claude::ClaudeBackend;
    use crate::backend::process::{Environment, Launcher};
    use crate::backend::{
        AccountRef, ApiKey, Backend, Credential, Event, FailureKind, Outcome, RunId, RunRequest,
        ToolPolicy, WorkerSandbox, run_temp,
    };
    use crate::paths::DataDir;

    /// A launcher whose environment is only `vars`, with `HOME` in `root`.
    fn launcher(root: &Path, vars: &[(&str, &str)]) -> Launcher {
        let mut env = Environment::empty();
        env.set("HOME", root);
        for (name, value) in vars {
            env.set(name, value);
        }
        Launcher::new(DataDir::new(root.join("data")).unwrap(), env)
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_missing_bwrap_or_socat_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let claude = script(&bin, "claude", "exit 0");
        let only = launcher(dir.path(), &[("PATH", bin.to_str().unwrap())]);

        let error = check_host(&only, &claude).await.unwrap_err();
        assert!(
            error.contains("bubblewrap (bwrap) isn't installed"),
            "{error}"
        );

        script(&bin, "bwrap", "exit 0");
        let error = check_host(&only, &claude).await.unwrap_err();
        assert!(error.contains("socat isn't installed"), "{error}");
    }

    /// A folder holding a `bwrap` and a `socat` that do nothing, and a launcher that finds them.
    fn fake_host() -> (tempfile::TempDir, PathBuf, Launcher) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(&bin, "bwrap", "exit 0");
        script(&bin, "socat", "exit 0");
        let launcher = launcher(dir.path(), &[("PATH", bin.to_str().unwrap())]);
        (dir, bin, launcher)
    }

    #[tokio::test]
    async fn a_claude_in_scrub_mode_or_with_an_unreadable_status_is_refused() {
        let (_dir, bin, only) = fake_host();
        let cases = [
            // Part of what Claude Code 2.1.283 prints with the flag on.
            (
                r#"echo '{"statusVersion":3,"autoAllowBashIfSandboxedSource":"unsupported"}'"#,
                "runs with CLAUDE_CODE_SUBPROCESS_ENV_SCRUB on",
            ),
            // Claude Code 2.1.274 and older.
            (
                r#"echo '{"statusVersion":2,"enabledSource":"off"}'"#,
                "update Claude Code to 2.1.275",
            ),
            (
                r#"echo '{"statusVersion":4,"autoAllowBashIfSandboxedSource":"default"}'"#,
                "plxd reads only version 3",
            ),
            (
                r#"echo '{"autoAllowBashIfSandboxedSource":"default"}'"#,
                "plxd reads only version 3",
            ),
            (
                r#"echo '{"statusVersion":3,"autoAllowBashIfSandboxedSource":"forced"}'"#,
                "reports autoAllowBashIfSandboxedSource as \"forced\"",
            ),
            (
                r#"echo '{"statusVersion":3}'"#,
                "reports autoAllowBashIfSandboxedSource as null",
            ),
            (
                r#"echo "error: unknown command 'sandbox'" >&2; exit 1"#,
                "failed (error: unknown command 'sandbox')",
            ),
            ("echo 'not json'", "printed something plxd can't read"),
        ];
        for (index, (body, expected)) in cases.into_iter().enumerate() {
            let claude = script(&bin, &format!("claude-{index}"), body);
            let error = check_host(&only, &claude).await.unwrap_err();
            assert!(error.contains(expected), "{body}: {error}");
        }
    }

    #[tokio::test]
    async fn the_status_check_gets_a_worker_s_environment() {
        let (dir, bin, _) = fake_host();
        // Fails the check if anything a worker doesn't get reaches it.
        let claude = script(
            &bin,
            "claude",
            r#"seen="${ANTHROPIC_BASE_URL+x}${CLAUDE_CONFIG_DIR+x}"
seen="$seen${CLAUDE_CODE_SUBPROCESS_ENV_SCRUB+x}"
[ -n "$seen" ] && { echo 'inherited a variable a worker never gets' >&2; exit 1; }
echo '{"statusVersion":3,"autoAllowBashIfSandboxedSource":"default"}'"#,
        );
        let inherited = launcher(
            dir.path(),
            &[
                ("PATH", bin.to_str().unwrap()),
                ("ANTHROPIC_BASE_URL", "https://example.invalid"),
                ("CLAUDE_CONFIG_DIR", "/elsewhere"),
                ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1"),
            ],
        );
        // The status passes, so the check moves on to the bwrap probe, which the fake fails. How
        // it words that depends on the host's AppArmor setting, but it always names bubblewrap.
        let error = check_host(&inherited, &claude).await.unwrap_err();
        assert!(error.contains("bubblewrap"), "{error}");
    }

    /// The real bwrap and socat, which CI installs along with the Claude Code it names in
    /// `PLX_SANDBOX_CLAUDE` (0013).
    fn installed() -> Option<(tempfile::TempDir, Launcher, PathBuf)> {
        let claude = PathBuf::from(std::env::var_os("PLX_SANDBOX_CLAUDE")?);
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path(), &[("PATH", INSTALLED_PATH)]);
        Some((dir, launcher, claude))
    }

    const INSTALLED_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

    /// A fake `claude`'s answer to `--restricted sandbox status` with the scrub flag off.
    const STATUS_OK: &str = r#"if [ "$1" = --restricted ]; then
    echo '{"statusVersion":3,"autoAllowBashIfSandboxedSource":"default"}'; exit 0
fi"#;

    #[tokio::test]
    async fn claude_code_s_own_filter_passes_the_check() {
        let Some((_dir, launcher, claude)) = installed() else {
            return;
        };
        check_host(&launcher, &claude).await.unwrap();
    }

    #[tokio::test]
    async fn a_claude_without_the_filter_is_refused() {
        let Some((dir, launcher, _)) = installed() else {
            return;
        };
        // Runs the command it is given with no filter, as a build without the helper would.
        let unfiltered = script(dir.path(), "claude", &format!("{STATUS_OK}\nexec \"$@\""));
        let error = check_host(&launcher, &unfiltered).await.unwrap_err();
        assert!(error.contains("let a sandboxed command connect"), "{error}");

        let broken = script(
            dir.path(),
            "broken-claude",
            &format!("{STATUS_OK}\necho 'no helper here' >&2; exit 1"),
        );
        let error = check_host(&launcher, &broken).await.unwrap_err();
        assert!(error.contains("could not run: no helper here"), "{error}");
    }

    #[tokio::test]
    async fn a_claude_with_the_scrub_flag_on_is_refused() {
        let Some((dir, _, claude)) = installed() else {
            return;
        };
        // In plxd's own environment the flag is dropped, as it is for a worker.
        let inherited = launcher(
            dir.path(),
            &[
                ("PATH", INSTALLED_PATH),
                ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1"),
            ],
        );
        check_host(&inherited, &claude).await.unwrap();

        // A managed settings `env` block puts the flag in Claude Code's own environment. A test
        // can't write /etc/claude-code as a normal user, and 2.1.283 ignores
        // CLAUDE_CODE_MANAGED_SETTINGS_PATH and CLAUDE_CODE_REMOTE_SETTINGS_PATH there, so the
        // flag goes straight into the status command's environment.
        let mut spec = status_spec(&inherited, claude.to_str().unwrap());
        spec.inject.set("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1");
        let error = read_status(&inherited, &spec).await.unwrap_err();
        assert!(
            error.contains("runs with CLAUDE_CODE_SUBPROCESS_ENV_SCRUB on"),
            "{error}"
        );
    }

    /// How a worker run through the real `claude`, via `wrapper`, ends: cancelled once its init
    /// passed plxd's checks, or failed. The wrapper points it at a closed port, so nothing
    /// reaches Anthropic.
    async fn worker_outcome(root: &Path, wrapper: &Path) -> Outcome {
        let data = root.join("data");
        let (worktree, context) = (data.join("worktrees/run"), data.join("context/p"));
        let git_dir = root.join("repo/.git");
        for folder in [&worktree, &context, &git_dir] {
            fs::create_dir_all(folder).unwrap();
        }
        let base = launcher(root, &[("PATH", INSTALLED_PATH)]);
        let temp = run_temp::create(base.data_dir()).unwrap();
        let backend = ClaudeBackend::new(base).with_program(wrapper);
        let request = RunRequest {
            run_id: RunId::generate(),
            turn_id: None,
            cwd: worktree.clone(),
            prompt: "Say hi.".into(),
            images: Vec::new(),
            policy: ToolPolicy::WorkspaceWrite,
            sandbox: Some(WorkerSandbox::for_worktree(
                root,
                &data,
                &worktree,
                &git_dir,
                &context,
                temp.path(),
            )),
            account: AccountRef {
                id: "test".into(),
                credential: Credential::ApiKey(ApiKey::new(
                    "sk-ant-parallax-test-not-a-key".into(),
                )),
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
        };
        let mut started = backend.start(request).unwrap();
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(60), started.events.next())
                    .await
                    .expect("no event within 60 s")
                    .expect("the stream ended before Finished");
            match event {
                // The init passed every check, and the CLI is now retrying the closed port.
                Event::Notice { .. } => started.run.cancel(),
                Event::Finished { outcome, .. } => return outcome,
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn a_worker_in_scrub_mode_fails_its_permission_mode_check() {
        let Some((dir, _, claude)) = installed() else {
            return;
        };
        let root = dir.path();
        let run = format!(
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:9 DISABLE_AUTOUPDATER=1 \
             CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\nexec '{}' \"$@\"",
            claude.display()
        );
        let off = script(root, "claude-off", &run);
        assert_eq!(
            worker_outcome(&root.join("off"), &off).await,
            Outcome::Cancelled
        );

        // Where a managed settings `env` block puts the flag: in Claude Code's own environment.
        let on = script(
            root,
            "claude-on",
            &format!("export CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1\n{run}"),
        );
        match worker_outcome(&root.join("on"), &on).await {
            Outcome::Failed(failure) => {
                assert_eq!(failure.failure, FailureKind::PolicyViolation, "{failure:?}");
                assert!(
                    failure.message.contains(r#"permission mode "default""#),
                    "{failure:?}"
                );
            }
            other => panic!("expected a policy violation, got {other:?}"),
        }
    }
}
