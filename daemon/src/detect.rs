//! Detecting installed vendor CLIs and their sign-in state (#114, decision record 0004).
//!
//! wispd never reads a CLI's credential files and never starts a sign-in. It only:
//!
//! - Resolves the binary on the `PATH` it itself uses ([`find_program`], #96), which runs
//!   nothing.
//! - Runs the read-only status commands 0004 lists, through the same [`Launcher`] and
//!   environment scrubbing the agent backends use, with a timeout.
//!
//! The exact shape of `claude auth status` and `agent status --format json` is undocumented, so
//! [`apply_json_status`] reads the fields these sources show and tolerates everything else,
//! mirroring 0007's forward-compatible parsing rule. A field wispd could not read is left `None`
//! rather than guessed, and [`DetectedCli::note`] says why when that happens.
//!
//! Plan/tier is available for Codex only through its `app-server`'s `account/read`, a JSON-RPC
//! server on stdio rather than a one-shot command. [`probe_codex_plan`] does the smallest useful
//! thing: one request, one response, then the process is killed. A real Codex backend (#122)
//! will want a proper client with its own handshake; this is not it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::Instant;
use wisp_protocol::{AuthKind, CliKind, DetectedCli};

use crate::backend::process::{Launcher, Output, ProcessSpec, StdinMode, find_program};

/// How long a status command may run before wispd gives up on it.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an `accounts/list` answer may be served from the cache before a fresh probe runs.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// The result of a probe: every detected CLI and when the probe that produced them ran. Matches
/// `AccountsListResult`/`AccountsRefreshResult`'s shape.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// Every CLI wispd knows how to detect, in a stable order (`claude`, `codex`, `cursor`).
    pub clis: Vec<DetectedCli>,
    /// When this probe ran.
    pub checked_at: Timestamp,
}

/// Detects `claude`, `codex`, and `agent` (Cursor), and caches the answer briefly.
#[derive(Debug)]
pub struct CliDetector {
    launcher: Launcher,
    timeout: Duration,
    cache: Mutex<Option<(Instant, Probe)>>,
}

impl CliDetector {
    /// A detector that probes through `launcher`, giving up on a status command after `timeout`,
    /// with an empty cache. Production code passes [`PROBE_TIMEOUT`]; tests use a shorter one.
    #[must_use]
    pub fn new(launcher: Launcher, timeout: Duration) -> Self {
        Self {
            launcher,
            timeout,
            cache: Mutex::new(None),
        }
    }

    /// The last probe, if one ran within [`CACHE_TTL`], else a fresh one.
    pub async fn list(&self) -> Probe {
        {
            let cache = self.cache.lock().await;
            if let Some((checked, probe)) = &*cache
                && checked.elapsed() < CACHE_TTL
            {
                return probe.clone();
            }
        }
        self.refresh().await
    }

    /// A fresh probe of every CLI. Updates the cache [`Self::list`] reads from.
    pub async fn refresh(&self) -> Probe {
        let (claude, codex, cursor) = tokio::join!(
            probe_claude(&self.launcher, self.timeout),
            probe_codex(&self.launcher, self.timeout),
            probe_cursor(&self.launcher, self.timeout),
        );
        let probe = Probe {
            clis: vec![claude, codex, cursor],
            checked_at: Timestamp::now(),
        };
        *self.cache.lock().await = Some((Instant::now(), probe.clone()));
        probe
    }
}

/// The result of running a status command to completion within the timeout.
struct Ran {
    stdout: String,
    stderr_tail: String,
    exit_code: Option<i32>,
}

/// Resolves `program` on `launcher`'s effective `PATH` (#96), without running it.
fn resolve(launcher: &Launcher, program: &str) -> Option<PathBuf> {
    let spec = ProcessSpec::new(program, "/");
    let env = launcher.environment(&spec);
    find_program(program.as_ref(), env.get("PATH")).ok()
}

fn not_installed(cli: CliKind) -> DetectedCli {
    DetectedCli {
        cli,
        installed: false,
        path: None,
        version: None,
        signed_in: None,
        auth_kind: None,
        plan: None,
        note: None,
    }
}

fn installed(cli: CliKind, path: &Path) -> DetectedCli {
    DetectedCli {
        cli,
        installed: true,
        path: Some(path.display().to_string()),
        version: None,
        signed_in: None,
        auth_kind: None,
        plan: None,
        note: None,
    }
}

/// Runs `program args` in `/`, with `launcher`'s scrubbing, stdin closed, and no output beyond
/// wispd's usual limits. Waits at most `timeout`; on a timeout, the process's group is killed
/// (dropping it does that) and `Err` explains why.
async fn run(
    launcher: &Launcher,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Ran, String> {
    let mut spec = ProcessSpec::new(program, "/");
    spec.args = args.iter().map(|arg| (*arg).into()).collect();
    let mut process = match launcher.spawn(&spec) {
        Ok(process) => process,
        Err(error) => return Err(format!("could not start {program}: {error}")),
    };
    let mut stdout = String::new();
    let mut exit_code = None;
    let mut stderr_tail = String::new();
    let collected = tokio::time::timeout(timeout, async {
        while let Some(output) = process.next().await {
            match output {
                Output::Line(bytes) => {
                    stdout.push_str(&String::from_utf8_lossy(&bytes));
                    stdout.push('\n');
                }
                Output::Oversized { .. } => {}
                Output::Exited(exit) => {
                    exit_code = exit.info.code;
                    stderr_tail = exit.stderr_tail;
                    break;
                }
            }
        }
    })
    .await;
    if collected.is_ok() {
        Ok(Ran {
            stdout,
            stderr_tail,
            exit_code,
        })
    } else {
        drop(process);
        Err(format!("timed out after {}s", timeout.as_secs()))
    }
}

/// Reads a status command's JSON object, tolerating unknown fields and outright garbage.
/// `signed_in`, `auth_kind`, `plan`, and `version` are read under several plausible key names,
/// since the real shape is undocumented (0004); an unparseable body falls back to 0004's
/// documented convention that exit 0 means signed in and exit 1 means signed out.
fn apply_json_status(detected: &mut DetectedCli, ran: &Ran) {
    let parsed = serde_json::from_str::<Value>(ran.stdout.trim()).ok();
    let Some(Value::Object(map)) = parsed else {
        fallback_from_exit_code(
            detected,
            ran.exit_code,
            "output wispd could not parse as JSON",
        );
        return;
    };
    let signed_in = ["loggedIn", "signedIn", "authenticated", "isAuthenticated"]
        .iter()
        .find_map(|key| map.get(*key))
        .and_then(Value::as_bool);
    detected.signed_in = signed_in.or_else(|| exit_code_signed_in(ran.exit_code));
    detected.version = map
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if detected.signed_in == Some(true) {
        detected.auth_kind = ["authType", "authKind", "loginType"]
            .iter()
            .find_map(|key| map.get(*key))
            .and_then(Value::as_str)
            .map_or(Some(AuthKind::Unknown), |value| {
                Some(parse_auth_kind(value))
            });
        detected.plan = ["subscriptionType", "plan", "tier"]
            .iter()
            .find_map(|key| map.get(*key))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
}

fn exit_code_signed_in(exit_code: Option<i32>) -> Option<bool> {
    match exit_code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn fallback_from_exit_code(detected: &mut DetectedCli, exit_code: Option<i32>, why: &str) {
    match exit_code_signed_in(exit_code) {
        Some(signed_in) => detected.signed_in = Some(signed_in),
        None => {
            detected.note = Some(match exit_code {
                Some(code) => format!("exited with code {code}; {why}"),
                None => format!("exited from a signal; {why}"),
            });
        }
    }
}

fn parse_auth_kind(text: &str) -> AuthKind {
    let text = text.to_ascii_lowercase();
    if text.contains("subscription") || text.contains("oauth") || text.contains("login") {
        AuthKind::Subscription
    } else if text.contains("key") || text.contains("token") {
        AuthKind::ApiKey
    } else {
        AuthKind::Unknown
    }
}

async fn probe_claude(launcher: &Launcher, timeout: Duration) -> DetectedCli {
    let Some(path) = resolve(launcher, "claude") else {
        return not_installed(CliKind::Claude);
    };
    let mut detected = installed(CliKind::Claude, &path);
    match run(launcher, "claude", &["auth", "status"], timeout).await {
        Ok(ran) => apply_json_status(&mut detected, &ran),
        Err(note) => detected.note = Some(note),
    }
    // #156 refuses a worker on a Claude Code older than the sandbox needs, so the version must be
    // known; `auth status` doesn't always report it, and `--version` does ("2.1.281 (Claude
    // Code)").
    if detected.version.is_none()
        && let Ok(ran) = run(launcher, "claude", &["--version"], timeout).await
        && ran.exit_code == Some(0)
    {
        detected.version = version_from_banner(&ran.stdout);
    }
    detected
}

/// The version number that starts a `--version` banner, such as `2.1.281` in `2.1.281 (Claude
/// Code)`.
fn version_from_banner(stdout: &str) -> Option<String> {
    let word = stdout.split_whitespace().next()?;
    crate::backend::claude::parse_version(word).map(|_| word.to_owned())
}

async fn probe_codex(launcher: &Launcher, timeout: Duration) -> DetectedCli {
    let Some(path) = resolve(launcher, "codex") else {
        return not_installed(CliKind::Codex);
    };
    let mut detected = installed(CliKind::Codex, &path);
    match run(launcher, "codex", &["login", "status"], timeout).await {
        Ok(ran) => {
            detected.signed_in = exit_code_signed_in(ran.exit_code);
            match detected.signed_in {
                Some(true) => {
                    detected.auth_kind = Some(AuthKind::Subscription);
                    detected.plan = probe_codex_plan(launcher, timeout).await;
                }
                Some(false) => {}
                None => {
                    let hint = ran.stderr_tail.lines().next().unwrap_or_default();
                    detected.note = Some(match ran.exit_code {
                        Some(code) => {
                            format!("`codex login status` exited with code {code}: {hint}")
                        }
                        None => "`codex login status` exited from a signal".to_owned(),
                    });
                }
            }
        }
        Err(note) => detected.note = Some(note),
    }
    detected
}

/// Asks a running `codex app-server` for the signed-in account's plan, over one NDJSON
/// request/response on stdio. Best-effort: any failure, including a malformed reply, is `None`
/// rather than an error, since a subscription's plan is metadata, not a fact wispd depends on.
/// The process is killed once this returns, whether or not it answered in time.
async fn probe_codex_plan(launcher: &Launcher, timeout: Duration) -> Option<String> {
    let mut spec = ProcessSpec::new("codex", "/");
    spec.args = vec!["app-server".into()];
    spec.stdin = StdinMode::Piped;
    let mut process = launcher.spawn(&spec).ok()?;
    let plan = tokio::time::timeout(timeout, async {
        let mut stdin = process.take_stdin()?;
        stdin
            .write_all(br#"{"id":1,"method":"account/read","params":{}}"#)
            .await
            .ok()?;
        stdin.write_all(b"\n").await.ok()?;
        while let Some(output) = process.next().await {
            let Output::Line(bytes) = output else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) != Some(1) {
                continue;
            }
            return value
                .pointer("/result/planType")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        None
    })
    .await
    .ok()
    .flatten();
    drop(process);
    plan
}

async fn probe_cursor(launcher: &Launcher, timeout: Duration) -> DetectedCli {
    let Some(path) = resolve(launcher, "agent") else {
        return not_installed(CliKind::Cursor);
    };
    let mut detected = installed(CliKind::Cursor, &path);
    match run(launcher, "agent", &["status", "--format", "json"], timeout).await {
        Ok(ran) => apply_json_status(&mut detected, &ran),
        Err(note) => detected.note = Some(note),
    }
    if detected.signed_in == Some(true)
        && let Ok(ran) = run(launcher, "agent", &["about"], timeout).await
    {
        detected.plan = extract_subscription_tier(&ran.stdout).or(detected.plan);
    }
    detected
}

/// Reads a `Subscription Tier: <value>` line from `agent about`'s plain text (0004, undocumented).
fn extract_subscription_tier(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let tier = line.trim().strip_prefix("Subscription Tier:")?.trim();
        (!tier.is_empty()).then(|| tier.to_owned())
    })
}

#[cfg(test)]
mod tests;
