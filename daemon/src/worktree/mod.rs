//! Git worktrees for agent runs: each subagent works in its own worktree of the project's repo
//! on the host, never in the user's own checkout.
//!
//! [`WorktreeManager`] runs every git command through [`Launcher`]: an explicit, scrubbed
//! environment and a timeout per call, so a hung or credential-prompting git can never block
//! wispd. Only [`WorktreeManager::accept`] changes the project repo's own working tree, and it
//! refuses rather than touch uncommitted changes (see `review`).
//!
//! A worktree lives at `<data dir>/worktrees/<repo slug>/<run id>`, where `<repo slug>` is the
//! repo's directory name plus a short hash of its canonical path. Its branch is
//! `wisp/<short run id>`, a hash of the run id: a prefix of a time-ordered `UUIDv7` would collide.
//!
//! [`WorktreeManager::create`] resolves `base` to a concrete commit once, so a later diff is
//! never compared against a ref that has since moved. With no explicit `base` it refuses a dirty
//! checkout, since a worktree cut from `HEAD` would silently drop those changes.
//! [`WorktreeManager::commit_all`] resolves `user.name`/`user.email` from the user's checkout and
//! scrubs every variable that could override them.
//!
//! # A worker's worktree is hostile input
//!
//! A run can write every file in its worktree, so the worktree's tracked content and git state
//! must never make a git process wispd runs there execute the worker's code: a hook, a
//! repository-configured `core.hooksPath` pointing at a tracked folder (as husky does), a
//! `.gitattributes` diff or filter driver, or a `.git` file rewritten to point elsewhere.
//!
//! [`WorktreeManager::create`] resolves the linked worktree's private git directory once, right
//! after `git worktree add`, the one moment its `.git` file is still trustworthy.
//! [`CreatedWorktree::git_dir`] carries it, and every later call scoped to the worktree pins
//! `--git-dir`/`--work-tree` to it, disables hooks, the pager, external diff and textconv
//! drivers, and remote helpers with `-c`, and runs with `GIT_CONFIG_NOSYSTEM`, a `/dev/null`
//! `GIT_CONFIG_GLOBAL`, and an empty dedicated `HOME`. The only config git can still read is the
//! pinned repository's own local config, which the worker cannot write.
//!
//! Calls in the user's own checkout (`create`, `remove`, `accept`) trust `repo_root`, but still
//! run with hooks off: once a run is accepted, the checkout's hooks can include files the worker
//! wrote.
//!
//! **Known gap:** filter drivers have no `-c` override, so an *absolute* `include.path` in the
//! pinned repository's local config that points into the worktree can still define one for a
//! worker's `.gitattributes` to trigger.

mod review;
mod scratch;
#[cfg(test)]
mod tests;

pub use review::{
    AcceptError, Accepted, Blob, CommitDiff, FileDiff, MAX_BLOB_BYTES, MergeHow, validate_repo_path,
};

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tracing::warn;
use wisp_protocol::RunId;

use crate::backend::process::{
    Environment, Exit, Launcher, Output, Process, ProcessSpec, SpawnError,
};

/// How long a single git invocation may run before wispd kills its process group.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Environment variables scrubbed from every git invocation, on top of
/// [`crate::backend::process::ALWAYS_SCRUBBED`]: anything that could redirect git to a different
/// repository or config, prompt for credentials, or override the commit identity we want to come
/// from the repo's own configuration.
const GIT_SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "EMAIL",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
];

/// Extra environment variables scrubbed from a call scoped to a worker's worktree, on top of
/// [`GIT_SCRUBBED`]: `XDG_CONFIG_HOME` could otherwise point git at a config file outside the
/// dedicated, empty `HOME` these calls inject.
const WORKTREE_GIT_EXTRA_SCRUBBED: &[&str] = &["XDG_CONFIG_HOME"];

/// The names, from `base`, of any `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` pair (git's way of
/// setting config from the environment, indexed rather than named, so [`GIT_SCRUBBED`] can't list
/// them). [`GIT_SCRUBBED`] already removes `GIT_CONFIG_COUNT`, without which git ignores every
/// indexed pair, so this is defense in depth in case something downstream sets its own count.
fn indexed_git_config_vars(base: &Environment) -> Vec<OsString> {
    base.names()
        .filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_")
        })
        .map(OsString::from)
        .collect()
}

/// `-c` overrides applied to every git command scoped to a worker's worktree, neutralizing
/// what repo-local config and tracked files can otherwise make git execute:
///
/// - `core.hooksPath=/dev/null` — no hooks run, wherever `core.hooksPath` points, including a
///   tracked folder such as husky's `.husky/_`. `--no-verify` alone only skips `pre-commit` and
///   `commit-msg`; `post-commit` and (on `git add`) `post-index-change` still run without this.
/// - `core.fsmonitor=false` — no filesystem monitor hook.
/// - `core.pager=cat`, `diff.external=` — no pager or external diff tool.
/// - `core.sshCommand=false` — if anything ever triggered a transport, no attacker-chosen SSH
///   command.
/// - `protocol.allow=never` — no remote helper protocol (for example `ext::`) runs.
///
/// Filter drivers (`filter.<name>.clean`/`.smudge`) can't be neutralized this way, because `-c`
/// needs the filter's name and `man git-config` gives no wildcard form. Instead, worktree-scoped
/// calls run with `GIT_CONFIG_NOSYSTEM=1`, a `/dev/null` `GIT_CONFIG_GLOBAL`, and a dedicated,
/// empty `HOME`, so the pinned repository's own local config, never worker-writable, is the only
/// place left a filter, or a diff or merge driver, could be configured.
const WORKTREE_GIT_CONFIG: &[(&str, &str)] = &[
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("core.pager", "cat"),
    ("core.sshCommand", "false"),
    ("diff.external", ""),
    ("protocol.allow", "never"),
];

/// Extra flags for a diff subcommand scoped to a worker's worktree: a `.gitattributes` `diff=`
/// driver's `textconv`, or an external diff, must not run either.
const NO_DIFF_DRIVERS: &[&str] = &["--no-ext-diff", "--no-textconv"];

/// Why a worktree operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// `repo_path` doesn't exist or isn't inside a git repository.
    #[error("{} is not a git repository: {detail}", .path.display())]
    NotAGitRepo { path: PathBuf, detail: String },
    /// `base` didn't resolve to a commit.
    #[error("could not resolve {reference:?} to a commit in {}: {detail}", .repo.display())]
    UnknownRevision {
        repo: PathBuf,
        reference: String,
        detail: String,
    },
    /// The default base (the repo's current branch `HEAD`) has uncommitted changes, which a new
    /// worktree cut from that commit would not include.
    #[error(
        "{} has uncommitted changes that a new worktree would not include; commit or stash them, or create the worktree from an explicit base",
        .repo.display()
    )]
    DirtyBase { repo: PathBuf },
    /// [`WorktreeManager::commit_all`] found changes to commit, but the repository has no
    /// `user.name` or `user.email` configured.
    #[error(
        "{} has no git identity configured (user.name and user.email); set one before an agent can commit there",
        .repo.display()
    )]
    MissingIdentity { repo: PathBuf },
    /// A git command exited with a non-zero status.
    #[error("`git {}` in {} failed: {detail}", .args.join(" "), .cwd.display())]
    GitFailed {
        cwd: PathBuf,
        /// The arguments after `git`.
        args: Vec<String>,
        /// The end of its stderr, or a description of its exit if stderr was empty.
        detail: String,
    },
    /// A git command did not finish within [`WorktreeManager`]'s timeout, and its process group
    /// was killed.
    #[error("`git {}` in {} timed out after {timeout:?}", .args.join(" "), .cwd.display())]
    Timeout {
        cwd: PathBuf,
        args: Vec<String>,
        timeout: Duration,
    },
    /// git could not be started.
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    /// A filesystem operation other than running git failed.
    #[error("could not use {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A newly created worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWorktree {
    /// Its absolute path, under the wisp-owned worktrees folder.
    pub path: PathBuf,
    /// Its branch, `wisp/<short run id>`.
    pub branch: String,
    /// The concrete commit it was created from, resolved once so it never moves under it.
    pub base: String,
    /// The linked worktree's own private git directory (`<repo>/.git/worktrees/<name>`),
    /// resolved once at creation, before any worker code has run. Every later call passes this
    /// back so it never has to trust the worker-writable `.git` file again.
    pub git_dir: PathBuf,
}

/// How a changed file differs from the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    /// Added since the base.
    Added,
    /// Modified since the base.
    Modified,
    /// Deleted since the base.
    Deleted,
    /// Renamed, with [`ChangedFile::old_path`] set.
    Renamed,
    /// Copied from another file, with [`ChangedFile::old_path`] set.
    Copied,
    /// Its type changed, for example a file became a symlink.
    TypeChanged,
    /// It has an unresolved merge conflict.
    Unmerged,
    /// A status letter this build doesn't recognize.
    Unknown(String),
}

/// One file that differs from the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// How it changed.
    pub status: ChangeStatus,
    /// Its current path, relative to the worktree root.
    pub path: String,
    /// Its path before a rename or copy.
    pub old_path: Option<String>,
}

/// How much a worktree differs from its base, from [`WorktreeManager::diff_stat`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    /// Files changed.
    pub files: u64,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
}

/// The commit [`WorktreeManager::commit_all`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Its full sha.
    pub sha: String,
}

/// Creates, inspects, and removes the worktrees agent runs use.
///
/// Cheap to clone: it shares its [`Launcher`] and its per-repository lock table.
#[derive(Clone)]
pub struct WorktreeManager {
    launcher: Launcher,
    root: PathBuf,
    /// `HOME` for every git command scoped to a worker's worktree: empty, so there is no
    /// `~/.gitconfig`, `~/.git-credentials`, or `~/.ssh` to route git through.
    git_safe_home: PathBuf,
    merge_timeout: Duration,
    repo_locks: Arc<StdMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>,
}

impl WorktreeManager {
    /// A manager whose worktrees live under `data_dir_root`'s `worktrees` folder, and whose git
    /// commands run through `launcher`.
    #[must_use]
    pub fn new(launcher: Launcher, data_dir_root: &Path) -> Self {
        Self {
            launcher,
            root: data_dir_root.join("worktrees"),
            git_safe_home: data_dir_root.join("git-safe-home"),
            merge_timeout: review::MERGE_TIMEOUT,
            repo_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Overrides how long Accept's checkout may run, 300 s by default.
    #[must_use]
    pub fn with_merge_timeout(mut self, merge_timeout: Duration) -> Self {
        self.merge_timeout = merge_timeout;
        self
    }

    /// Creates a worktree of `repo_path` for `run_id`, on a new branch `wisp/<short run id>`
    /// starting from `base` (a commit-ish git can resolve), or the repo's current branch `HEAD`
    /// when `base` is `None`.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::NotAGitRepo`] if `repo_path` isn't a git repository,
    /// [`WorktreeError::UnknownRevision`] if `base` doesn't resolve,
    /// [`WorktreeError::DirtyBase`] if `base` was left unset and the repo has uncommitted
    /// changes, or [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or
    /// [`WorktreeError::Spawn`] from running git.
    pub async fn create(
        &self,
        repo_path: &Path,
        run_id: RunId,
        base: Option<&str>,
    ) -> Result<CreatedWorktree, WorktreeError> {
        let repo_root = self.repo_root(repo_path).await?;
        let _guard = self.lock_repo(&repo_root).await;

        let resolved_base = if let Some(reference) = base {
            self.resolve_commit(&repo_root, reference).await?
        } else {
            if self.is_dirty(&repo_root).await? {
                return Err(WorktreeError::DirtyBase { repo: repo_root });
            }
            self.resolve_commit(&repo_root, "HEAD").await?
        };

        let branch = format!("wisp/{}", short_hash(&run_id.to_string()));
        let path = self
            .root
            .join(project_dir_name(&repo_root))
            .join(run_id.to_string());
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| WorktreeError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
        }

        let path_arg = path.to_string_lossy().into_owned();
        self.run_git_ok(
            &repo_root,
            &["worktree", "add", "-b", &branch, &path_arg, &resolved_base],
        )
        .await?;

        // The one moment the new worktree's `.git` file is trusted: git just wrote it, and no
        // worker has run yet. Every later call pins this path explicitly instead.
        let git_dir_output = self
            .run_git_ok(&path, &["rev-parse", "--absolute-git-dir"])
            .await?;
        let git_dir = PathBuf::from(git_dir_output.trim());

        Ok(CreatedWorktree {
            path,
            branch,
            base: resolved_base,
            git_dir,
        })
    }

    /// How many files and lines differ between `base` and the worktree's current state, committed
    /// or not: `git diff --numstat`, pinned and hardened. A binary file counts as a changed file
    /// with no lines.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn diff_stat(
        &self,
        worktree_path: &Path,
        git_dir: &Path,
        base: &str,
    ) -> Result<DiffStat, WorktreeError> {
        self.stage_all(worktree_path, git_dir).await?;
        let mut args = vec!["diff", "--cached", "--no-color", "--find-renames"];
        args.extend_from_slice(NO_DIFF_DRIVERS);
        args.extend_from_slice(&["--numstat", base]);
        let output = self
            .run_worktree_git_ok(worktree_path, git_dir, &args)
            .await?;
        Ok(parse_numstat(&output))
    }

    /// The shared git folder of the repository at `repo_path` (`git rev-parse --git-common-dir`),
    /// as an absolute path. It runs in the user's own checkout, never in a worker's worktree, so
    /// no worker-written file decides the answer. The worker sandbox makes it read-only.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::NotAGitRepo`], or [`WorktreeError::GitFailed`],
    /// [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn git_common_dir(&self, repo_path: &Path) -> Result<PathBuf, WorktreeError> {
        let repo_root = self.repo_root(repo_path).await?;
        let output = self
            .run_git_ok(
                &repo_root,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
        Ok(PathBuf::from(output.trim()))
    }

    /// Stages every change in the worktree and commits it with `message`, using the repository's
    /// own configured `user.name`/`user.email`, resolved from `repo_root` (see
    /// [`WorktreeManager::resolve_identity`]). Returns `None`, committing nothing, if there is
    /// nothing to commit.
    ///
    /// Runs with `--no-verify` and `--no-gpg-sign`: hooks and interactive signing assume a person
    /// is at the keyboard, and a headless commit that triggers either must not hang wispd.
    /// `--no-verify` alone only skips the `pre-commit` and `commit-msg` hooks; `git_dir` must be
    /// [`CreatedWorktree::git_dir`] for `worktree_path`, which additionally disables every other
    /// hook and execution vector a worker's worktree could reach.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::MissingIdentity`] if there is something to commit but `repo_root` has no
    /// configured identity, or [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or
    /// [`WorktreeError::Spawn`].
    pub async fn commit_all(
        &self,
        worktree_path: &Path,
        git_dir: &Path,
        repo_root: &Path,
        message: &str,
    ) -> Result<Option<Commit>, WorktreeError> {
        self.stage_all(worktree_path, git_dir).await?;
        let staged = self
            .run_worktree_git_ok(worktree_path, git_dir, &["diff", "--cached", "--name-only"])
            .await?;
        if staged.trim().is_empty() {
            return Ok(None);
        }
        let Some((name, email)) = self.resolve_identity(repo_root).await? else {
            return Err(WorktreeError::MissingIdentity {
                repo: repo_root.to_owned(),
            });
        };
        let user_name_arg = format!("user.name={name}");
        let user_email_arg = format!("user.email={email}");
        self.run_worktree_git_ok(
            worktree_path,
            git_dir,
            &[
                "-c",
                user_name_arg.as_str(),
                "-c",
                user_email_arg.as_str(),
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "--message",
                message,
            ],
        )
        .await?;
        let sha = self
            .run_worktree_git_ok(worktree_path, git_dir, &["rev-parse", "HEAD"])
            .await?;
        Ok(Some(Commit {
            sha: sha.trim().to_owned(),
        }))
    }

    /// Removes `worktree_path` and its `branch` from `repo_path`'s repository.
    ///
    /// Branch deletion is best-effort: the worktree is gone either way, and a branch that is
    /// already gone, or that git otherwise refuses to delete, only produces a log warning.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::NotAGitRepo`], or [`WorktreeError::GitFailed`],
    /// [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`] removing the worktree itself.
    pub async fn remove(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
        branch: &str,
    ) -> Result<(), WorktreeError> {
        let repo_root = self.repo_root(repo_path).await?;
        let _guard = self.lock_repo(&repo_root).await;

        let path_arg = worktree_path.to_string_lossy().into_owned();
        self.run_git_ok(&repo_root, &["worktree", "remove", "--force", &path_arg])
            .await?;

        match self.run_git(&repo_root, &["branch", "-D", branch]).await {
            Ok(output) if output.success() => {}
            Ok(output) => warn!(
                branch,
                repo = %repo_root.display(),
                stderr = %output.exit.stderr_tail,
                "could not delete a removed worktree's branch"
            ),
            Err(error) => {
                warn!(branch, repo = %repo_root.display(), %error, "could not delete a removed worktree's branch");
            }
        }
        let _ = self.run_git(&repo_root, &["worktree", "prune"]).await;
        Ok(())
    }

    /// `repo_path`'s repository root, and confirmation that it is one.
    async fn repo_root(&self, repo_path: &Path) -> Result<PathBuf, WorktreeError> {
        let not_a_repo = |detail: String| WorktreeError::NotAGitRepo {
            path: repo_path.to_owned(),
            detail,
        };
        let canonical = tokio::fs::canonicalize(repo_path)
            .await
            .map_err(|source| not_a_repo(source.to_string()))?;
        let output = self
            .run_git(&canonical, &["rev-parse", "--show-toplevel"])
            .await?;
        if !output.success() {
            return Err(not_a_repo(describe_failure(&output)));
        }
        Ok(PathBuf::from(output.stdout.trim().to_owned()))
    }

    async fn resolve_commit(
        &self,
        repo_root: &Path,
        reference: &str,
    ) -> Result<String, WorktreeError> {
        let commit_ish = format!("{reference}^{{commit}}");
        let output = self
            .run_git(
                repo_root,
                &["rev-parse", "--verify", "--quiet", &commit_ish],
            )
            .await?;
        let sha = output.stdout.trim();
        if !output.success() || sha.is_empty() {
            return Err(WorktreeError::UnknownRevision {
                repo: repo_root.to_owned(),
                reference: reference.to_owned(),
                detail: describe_failure(&output),
            });
        }
        Ok(sha.to_owned())
    }

    /// Stages every change in the worktree, including untracked files: plain `git diff <base>`
    /// never shows an untracked file, so [`WorktreeManager::diff_stat`] and
    /// [`WorktreeManager::commit_all`] compare `base` against the index (`--cached`) after
    /// staging. That gives the same answer before and after a run's changes are committed.
    async fn stage_all(&self, worktree_path: &Path, git_dir: &Path) -> Result<(), WorktreeError> {
        self.run_worktree_git_ok(worktree_path, git_dir, &["add", "-A"])
            .await?;
        Ok(())
    }

    async fn is_dirty(&self, repo_root: &Path) -> Result<bool, WorktreeError> {
        let status = self
            .run_git_ok(repo_root, &["status", "--porcelain"])
            .await?;
        Ok(!status.trim().is_empty())
    }

    /// The repository's configured `user.name`/`user.email`, or `None` if either is unset.
    ///
    /// Resolved against `repo_root`, the user's own checkout, through the unrestricted
    /// [`WorktreeManager::run_git`]: an identity configured only in `~/.gitconfig`, true for most
    /// users, must still resolve, and the worktree-scoped commit's own environment can't see it.
    async fn resolve_identity(
        &self,
        repo_root: &Path,
    ) -> Result<Option<(String, String)>, WorktreeError> {
        let configured = |output: GitOutput| -> Option<String> {
            if !output.success() {
                return None;
            }
            let value = output.stdout.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let name = self
            .run_git(repo_root, &["config", "--get", "user.name"])
            .await?;
        let email = self
            .run_git(repo_root, &["config", "--get", "user.email"])
            .await?;
        Ok(match (configured(name), configured(email)) {
            (Some(name), Some(email)) => Some((name, email)),
            _ => None,
        })
    }

    async fn lock_repo(&self, repo_root: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut locks = self
                .repo_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                locks
                    .entry(repo_root.to_owned())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
            )
        };
        mutex.lock_owned().await
    }

    /// Runs `git args` in `cwd` and returns its output, whatever its exit status.
    ///
    /// Every call runs with `core.hooksPath=/dev/null`: no git command wispd runs in the user's
    /// checkout ever runs a repository hook. Once a run is accepted, the repository's hooks can
    /// include files the agent wrote (a tracked `core.hooksPath` such as husky's `.husky/`), and
    /// `worktree add` (`post-checkout`) or `branch -D` (`reference-transaction`) would otherwise
    /// run them, headless and unsandboxed, in wispd.
    async fn run_git(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, WorktreeError> {
        self.run_git_for(cwd, args, TIMEOUT).await
    }

    /// [`WorktreeManager::run_git`] with its own timeout, for the one call that may take long.
    async fn run_git_for(
        &self,
        cwd: &Path,
        args: &[&str],
        limit: Duration,
    ) -> Result<GitOutput, WorktreeError> {
        let mut spec = ProcessSpec::new("git", cwd);
        spec.args = ["-c", "core.hooksPath=/dev/null"]
            .iter()
            .chain(args)
            .map(|arg| OsString::from(*arg))
            .collect();
        spec.scrub = GIT_SCRUBBED
            .iter()
            .map(|name| OsString::from(*name))
            .collect();
        spec.inject.set("GIT_TERMINAL_PROMPT", "0");
        self.spawn_collect(&spec, args, limit)
            .await
            .map(GitOutput::new)
    }

    /// Like [`WorktreeManager::run_git`], but a non-zero exit becomes [`WorktreeError::GitFailed`]
    /// and only stdout is returned.
    async fn run_git_ok(&self, cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
        self.run_git(cwd, args).await?.ok(cwd, args)
    }

    /// Runs `git args` against `work_tree`, pinned to `git_dir` and hardened: see the module
    /// documentation and [`WORKTREE_GIT_CONFIG`]. A non-zero exit becomes
    /// [`WorktreeError::GitFailed`], and only stdout is returned.
    async fn run_worktree_git_ok(
        &self,
        work_tree: &Path,
        git_dir: &Path,
        args: &[&str],
    ) -> Result<String, WorktreeError> {
        let spec = self.worktree_spec(work_tree, git_dir, args).await?;
        self.spawn_collect(&spec, args, TIMEOUT)
            .await
            .map(GitOutput::new)?
            .ok(work_tree, args)
    }

    /// The process spec for `git args` scoped to a worker's worktree: see
    /// [`Self::run_worktree_git_ok`].
    async fn worktree_spec(
        &self,
        work_tree: &Path,
        git_dir: &Path,
        args: &[&str],
    ) -> Result<ProcessSpec, WorktreeError> {
        tokio::fs::create_dir_all(&self.git_safe_home)
            .await
            .map_err(|source| WorktreeError::Io {
                path: self.git_safe_home.clone(),
                source,
            })?;

        let mut spec = ProcessSpec::new("git", work_tree);
        spec.args = worktree_argv(work_tree, git_dir, args);
        spec.scrub = GIT_SCRUBBED
            .iter()
            .chain(WORKTREE_GIT_EXTRA_SCRUBBED)
            .map(|name| OsString::from(*name))
            .chain(indexed_git_config_vars(self.launcher.base()))
            .collect();
        spec.inject.set("GIT_TERMINAL_PROMPT", "0");
        spec.inject.set("GIT_CONFIG_NOSYSTEM", "1");
        spec.inject.set("GIT_CONFIG_GLOBAL", "/dev/null");
        spec.inject
            .set("HOME", self.git_safe_home.to_string_lossy().into_owned());
        Ok(spec)
    }

    /// Runs `spec` to completion and returns its stdout and exit, or [`WorktreeError::Timeout`]
    /// (reporting `args`) once `limit` passes.
    async fn spawn_collect(
        &self,
        spec: &ProcessSpec,
        args: &[&str],
        limit: Duration,
    ) -> Result<(Vec<u8>, Exit), WorktreeError> {
        let process = self.launcher.spawn(spec)?;
        timeout(limit, collect(process))
            .await
            .map_err(|_| timed_out(&spec.cwd, args, limit))
    }
}

/// Builds the full argument list for a git command scoped to a worker's worktree:
/// `--git-dir`/`--work-tree` pinned explicitly, ahead of any auto-discovery from a `.git` file,
/// then [`WORKTREE_GIT_CONFIG`]'s `-c` overrides, then `args` as the caller gave them.
fn worktree_argv(work_tree: &Path, git_dir: &Path, args: &[&str]) -> Vec<OsString> {
    let mut full_args = Vec::with_capacity(2 + WORKTREE_GIT_CONFIG.len() * 2 + args.len());
    full_args.push(OsString::from(format!("--git-dir={}", git_dir.display())));
    full_args.push(OsString::from(format!(
        "--work-tree={}",
        work_tree.display()
    )));
    for (key, value) in WORKTREE_GIT_CONFIG {
        full_args.push(OsString::from("-c"));
        full_args.push(OsString::from(format!("{key}={value}")));
    }
    full_args.extend(args.iter().map(|arg| OsString::from(*arg)));
    full_args
}

/// The result of running one git command to completion.
struct GitOutput {
    stdout: String,
    exit: Exit,
}

impl GitOutput {
    fn new((stdout, exit): (Vec<u8>, Exit)) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            exit,
        }
    }

    fn success(&self) -> bool {
        self.exit.info.success()
    }

    /// Its stdout, or [`WorktreeError::GitFailed`] for a non-zero exit.
    fn ok(self, cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
        if self.success() {
            Ok(self.stdout)
        } else {
            Err(git_failed(cwd, args, describe_failure(&self)))
        }
    }
}

fn describe_failure(output: &GitOutput) -> String {
    if output.exit.stderr_tail.is_empty() {
        format!("exited {:?} with no stderr", output.exit.info)
    } else {
        output.exit.stderr_tail.clone()
    }
}

fn owned_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn git_failed(cwd: &Path, args: &[&str], detail: String) -> WorktreeError {
    WorktreeError::GitFailed {
        cwd: cwd.to_owned(),
        args: owned_args(args),
        detail,
    }
}

fn timed_out(cwd: &Path, args: &[&str], limit: Duration) -> WorktreeError {
    WorktreeError::Timeout {
        cwd: cwd.to_owned(),
        args: owned_args(args),
        timeout: limit,
    }
}

/// Reads every line of `process`'s stdout until it exits, joining lines back with `\n`. The exact
/// framing of the original bytes doesn't matter here: every caller either parses the result line
/// by line or trims it as one block of text.
async fn collect(mut process: Process) -> (Vec<u8>, Exit) {
    let mut stdout = Vec::new();
    loop {
        match process.next().await {
            Some(Output::Line(line)) => {
                stdout.extend_from_slice(&line);
                stdout.push(b'\n');
            }
            Some(Output::Oversized { .. }) => {}
            Some(Output::Exited(exit)) => return (stdout, exit),
            None => unreachable!("Output::Exited always comes last"),
        }
    }
}

/// The first 8 hex digits of the SHA-256 of `text`. Used for both the short run id in a branch
/// name and the repository hash in a worktree folder's name; a prefix of a `UUIDv7` would cluster
/// collisions in time, since most of a `UUIDv7`'s own bits are a timestamp.
fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..4].iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// The `<repo slug>` folder name for `repo_root`: its own directory name, sanitized, plus a short
/// hash of its full path, so differently located repos that share a name never collide.
fn project_dir_name(repo_root: &Path) -> String {
    let basename = repo_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sanitized: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    let sanitized = if sanitized.is_empty() {
        "repo"
    } else {
        sanitized
    };
    format!("{sanitized}-{}", short_hash(&repo_root.to_string_lossy()))
}

/// Sums `git diff --numstat`'s output: `added\tdeleted\tpath` per file, with `-` for both counts
/// of a binary file.
fn parse_numstat(output: &str) -> DiffStat {
    let mut stat = DiffStat::default();
    for line in output.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let mut count = || {
            fields
                .next()
                .and_then(|field| field.parse::<u64>().ok())
                .unwrap_or(0)
        };
        stat.insertions += count();
        stat.deletions += count();
        stat.files += 1;
    }
    stat
}
