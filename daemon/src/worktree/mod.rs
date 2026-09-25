//! Git worktrees for agent runs (#154, plan #67): each subagent works in its own worktree of the
//! project's repo on the host, never in the user's own checkout.
//!
//! [`WorktreeManager`] runs every git command through [`Launcher`], the same process supervisor
//! backends use: an explicit, scrubbed environment and a timeout per call, so a hung or
//! credential-prompting git can never block wispd. It never runs a git command whose `cwd` is the
//! project repo's own working tree in a way that could change it: `worktree add`, `worktree
//! remove`, and `worktree prune` only touch `.git/worktrees` metadata and refs, and `status`,
//! `rev-parse`, and `diff` are read-only.
//!
//! This module is not wired to the protocol yet; #156 (`agent/start` end to end) is the first
//! caller, and #157 is what a client sees. Storing a worktree's row and deciding when to persist
//! it is left to that caller: `wisp-store`'s blocking SQLite calls already run on
//! [`crate::store::StoreHandle`]'s own thread for everything else the async server touches, and
//! #156 owns wiring the run lifecycle (and its own runs/events tables) into that. What this module
//! gives #156 is the `worktrees` table and CRUD (`wisp_store::Store::{create,get,list,delete}_worktree`)
//! and a [`WorktreeManager`] ready to be called with whatever path set the store produces.
//!
//! # Layout and naming
//!
//! A worktree lives at `<data dir>/worktrees/<repo slug>/<run id>`, where `<repo slug>` is the
//! repo's directory name plus a short hash of its canonical path (so two repos named the same
//! thing never collide, and the folder stays readable). Its branch is `wisp/<short run id>`,
//! `<short run id>` being the first 8 hex digits of the SHA-256 of the run id — the same
//! short-hash idea [`crate::paths::DataDir`] uses for its socket fallback, and collision-free in
//! the way a prefix of the run id's own (time-ordered) `UUIDv7` bytes would not be.
//!
//! # Base and identity
//!
//! [`WorktreeManager::create`] resolves `base` to a concrete commit once, at creation, so a later
//! [`WorktreeManager::diff`] is never compared against a ref that has since moved. When the caller
//! leaves `base` unset (today's only case: the repo's current branch `HEAD`), creation refuses if
//! the repo's working tree is dirty, since a new worktree cut from `HEAD` would silently drop
//! those uncommitted changes; an explicit `base` skips that check. [`WorktreeManager::commit_all`]
//! scrubs every environment variable that could override the repo's configured
//! `user.name`/`user.email` (`GIT_AUTHOR_*`, `GIT_COMMITTER_*`, `EMAIL`), so a commit is always
//! attributed to whatever the repo itself says, never to whatever wispd inherited.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
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
    Exit, Launcher, Output, Process, ProcessSpec, SpawnError, StdinMode,
};

/// How long a single git invocation may run before wispd gives up on it and kills its process
/// group. A hung `git` (an unexpected credential prompt, a stuck hook) must never block wispd.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes of unified diff [`WorktreeManager::diff`] returns before truncating. Diffs,
/// like everything else, come through paged or capped methods (decision record 0007).
pub const DEFAULT_MAX_DIFF_BYTES: usize = 1024 * 1024;

const WORKTREES_DIR: &str = "worktrees";

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

/// Why a worktree operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// `repo_path` doesn't exist or isn't inside a git repository.
    #[error("{} is not a git repository: {detail}", .path.display())]
    NotAGitRepo {
        /// The path as given.
        path: PathBuf,
        /// What git or the filesystem said.
        detail: String,
    },
    /// `base` didn't resolve to a commit.
    #[error("could not resolve {reference:?} to a commit in {}: {detail}", .repo.display())]
    UnknownRevision {
        /// The repository.
        repo: PathBuf,
        /// The reference as given.
        reference: String,
        /// What git said.
        detail: String,
    },
    /// The default base (the repo's current branch `HEAD`) has uncommitted changes, which a new
    /// worktree cut from that commit would not include.
    #[error(
        "{} has uncommitted changes that a new worktree would not include; commit or stash them, or create the worktree from an explicit base",
        .repo.display()
    )]
    DirtyBase {
        /// The repository.
        repo: PathBuf,
    },
    /// [`WorktreeManager::commit_all`] found changes to commit, but the repository has no
    /// `user.name` or `user.email` configured.
    #[error(
        "{} has no git identity configured (user.name and user.email); set one before an agent can commit there",
        .repo.display()
    )]
    MissingIdentity {
        /// The repository (or worktree) that lacks an identity.
        repo: PathBuf,
    },
    /// A git command exited with a non-zero status.
    #[error("`git {}` in {} failed: {detail}", .args.join(" "), .cwd.display())]
    GitFailed {
        /// Where it ran.
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
        /// Where it ran.
        cwd: PathBuf,
        /// The arguments after `git`.
        args: Vec<String>,
        /// The timeout that elapsed.
        timeout: Duration,
    },
    /// git could not be started.
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    /// A filesystem operation other than running git failed.
    #[error("could not use {}: {source}", .path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
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

/// A unified diff, possibly cut short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// The diff text.
    pub text: String,
    /// Whether `text` was cut short of the full diff.
    pub truncated: bool,
}

/// The commit [`WorktreeManager::commit_all`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Its full sha.
    pub sha: String,
}

/// What [`WorktreeManager::gc_orphans`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Worktree folders it removed.
    pub removed: Vec<PathBuf>,
    /// Folders it could not remove, and why.
    pub errors: Vec<(PathBuf, String)>,
}

/// Creates, inspects, and removes the worktrees agent runs use.
///
/// Cheap to clone: it shares its [`Launcher`] and its per-repository lock table.
#[derive(Clone)]
pub struct WorktreeManager {
    launcher: Launcher,
    root: PathBuf,
    timeout: Duration,
    max_diff_bytes: usize,
    repo_locks: Arc<StdMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>,
}

impl WorktreeManager {
    /// A manager whose worktrees live under `data_dir_root`'s `worktrees` folder, and whose git
    /// commands run through `launcher`.
    #[must_use]
    pub fn new(launcher: Launcher, data_dir_root: &Path) -> Self {
        Self {
            launcher,
            root: data_dir_root.join(WORKTREES_DIR),
            timeout: DEFAULT_TIMEOUT,
            max_diff_bytes: DEFAULT_MAX_DIFF_BYTES,
            repo_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Overrides the per-command timeout, [`DEFAULT_TIMEOUT`] by default.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Overrides the diff size cap, [`DEFAULT_MAX_DIFF_BYTES`] by default.
    #[must_use]
    pub fn with_max_diff_bytes(mut self, max_diff_bytes: usize) -> Self {
        self.max_diff_bytes = max_diff_bytes;
        self
    }

    /// The wisp-owned folder every worktree lives under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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

        Ok(CreatedWorktree {
            path,
            branch,
            base: resolved_base,
        })
    }

    /// Files that differ between `base` (a commit git can resolve, normally
    /// [`CreatedWorktree::base`]) and the worktree's current state, committed or not.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn changed_files(
        &self,
        worktree_path: &Path,
        base: &str,
    ) -> Result<Vec<ChangedFile>, WorktreeError> {
        self.stage_all(worktree_path).await?;
        let output = self
            .run_git_ok(
                worktree_path,
                &[
                    "diff",
                    "--cached",
                    "--no-color",
                    "--find-renames",
                    "--name-status",
                    base,
                ],
            )
            .await?;
        Ok(parse_name_status(&output))
    }

    /// A unified diff between `base` and the worktree's current state, committed or not, cut off
    /// at this manager's diff size cap.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn diff(&self, worktree_path: &Path, base: &str) -> Result<Diff, WorktreeError> {
        self.stage_all(worktree_path).await?;
        let output = self
            .run_git_ok(
                worktree_path,
                &["diff", "--cached", "--no-color", "--find-renames", base],
            )
            .await?;
        Ok(cap_diff(output, self.max_diff_bytes))
    }

    /// Stages every change in the worktree and commits it with `message`, using the repository's
    /// own configured `user.name`/`user.email`. Returns `None`, committing nothing, if there is
    /// nothing to commit.
    ///
    /// Runs with `--no-verify` and `--no-gpg-sign`: hooks and interactive signing assume a person
    /// is at the keyboard, and a headless commit that triggers either must not hang wispd.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::MissingIdentity`] if there is something to commit but the repository has
    /// no configured identity, or [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or
    /// [`WorktreeError::Spawn`].
    pub async fn commit_all(
        &self,
        worktree_path: &Path,
        message: &str,
    ) -> Result<Option<Commit>, WorktreeError> {
        self.stage_all(worktree_path).await?;
        let staged = self
            .run_git_ok(worktree_path, &["diff", "--cached", "--name-only"])
            .await?;
        if staged.trim().is_empty() {
            return Ok(None);
        }
        if !self.has_identity(worktree_path).await? {
            return Err(WorktreeError::MissingIdentity {
                repo: worktree_path.to_owned(),
            });
        }
        self.run_git_ok(
            worktree_path,
            &[
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "--message",
                message,
            ],
        )
        .await?;
        let sha = self
            .run_git_ok(worktree_path, &["rev-parse", "HEAD"])
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

    /// Removes every worktree folder under [`WorktreeManager::root`] that isn't in `known`
    /// (normally every [`wisp_store::Worktree::path`] the store has), and cleans up any project
    /// folder that becomes empty as a result.
    ///
    /// A folder that is still a valid linked worktree is removed properly, through the
    /// repository it belongs to (`git worktree remove`, then `git worktree prune`, so the
    /// repository's own bookkeeping never keeps a dangling entry). Anything else there, such as a
    /// folder left behind after its repository disappeared, is removed directly.
    pub async fn gc_orphans(&self, known: &HashSet<PathBuf>) -> GcReport {
        let mut report = GcReport::default();
        let project_dirs = match read_dir_entries(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return report,
            Err(error) => {
                report.errors.push((self.root.clone(), error.to_string()));
                return report;
            }
        };

        for project_dir in project_dirs {
            if !matches!(tokio::fs::metadata(&project_dir).await, Ok(meta) if meta.is_dir()) {
                continue;
            }
            let run_dirs = match read_dir_entries(&project_dir).await {
                Ok(entries) => entries,
                Err(error) => {
                    report.errors.push((project_dir, error.to_string()));
                    continue;
                }
            };

            let mut project_now_empty = true;
            for run_dir in run_dirs {
                if known.contains(&run_dir) {
                    project_now_empty = false;
                    continue;
                }
                match self.remove_orphan(&run_dir).await {
                    Ok(()) => report.removed.push(run_dir),
                    Err(error) => {
                        report.errors.push((run_dir, error.to_string()));
                        project_now_empty = false;
                    }
                }
            }
            if project_now_empty {
                let _ = tokio::fs::remove_dir(&project_dir).await;
            }
        }
        report
    }

    async fn remove_orphan(&self, path: &Path) -> Result<(), WorktreeError> {
        if let Some(repo_root) = self.find_repo_of_worktree(path).await {
            let _guard = self.lock_repo(&repo_root).await;
            let path_arg = path.to_string_lossy().into_owned();
            if self
                .run_git_ok(&repo_root, &["worktree", "remove", "--force", &path_arg])
                .await
                .is_ok()
            {
                let _ = self.run_git(&repo_root, &["worktree", "prune"]).await;
                return Ok(());
            }
        }
        // Not a linked worktree wispd can still reach through its repository (the repository is
        // gone, or the folder was never a worktree at all): remove it directly.
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|source| WorktreeError::Io {
                path: path.to_owned(),
                source,
            })
    }

    /// The main repository a linked worktree at `path` belongs to, if `path` is still a valid
    /// linked worktree.
    async fn find_repo_of_worktree(&self, path: &Path) -> Option<PathBuf> {
        let output = self
            .run_git(
                path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await
            .ok()?;
        if !output.success() {
            return None;
        }
        let git_dir = PathBuf::from(output.stdout.trim());
        git_dir.parent().map(Path::to_path_buf)
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
    /// never shows an untracked file, so [`WorktreeManager::changed_files`],
    /// [`WorktreeManager::diff`], and [`WorktreeManager::commit_all`] all compare `base` against
    /// the index (`--cached`) after staging, rather than the working tree directly. That gives the
    /// same answer before and after a run's changes are committed: once committed, staging finds
    /// nothing new, and the index already matches `HEAD`.
    async fn stage_all(&self, worktree_path: &Path) -> Result<(), WorktreeError> {
        self.run_git_ok(worktree_path, &["add", "-A"]).await?;
        Ok(())
    }

    async fn is_dirty(&self, repo_root: &Path) -> Result<bool, WorktreeError> {
        let status = self
            .run_git_ok(repo_root, &["status", "--porcelain"])
            .await?;
        Ok(!status.trim().is_empty())
    }

    async fn has_identity(&self, cwd: &Path) -> Result<bool, WorktreeError> {
        let configured = |output: GitOutput| output.success() && !output.stdout.trim().is_empty();
        let name = self.run_git(cwd, &["config", "--get", "user.name"]).await?;
        let email = self
            .run_git(cwd, &["config", "--get", "user.email"])
            .await?;
        Ok(configured(name) && configured(email))
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

    /// Runs `git args` in `cwd` and returns its output, whatever its exit status. Only
    /// [`WorktreeError::Spawn`] and [`WorktreeError::Timeout`] are possible failures here; callers
    /// that want a non-zero exit turned into an error use [`WorktreeManager::run_git_ok`].
    async fn run_git(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, WorktreeError> {
        let mut spec = ProcessSpec::new("git", cwd);
        spec.args = args.iter().map(|arg| OsString::from(*arg)).collect();
        spec.scrub = GIT_SCRUBBED
            .iter()
            .map(|name| OsString::from(*name))
            .collect();
        spec.inject.set("GIT_TERMINAL_PROMPT", "0");
        spec.stdin = StdinMode::Null;

        let process = self.launcher.spawn(&spec)?;
        match timeout(self.timeout, collect(process)).await {
            Ok((stdout, exit)) => Ok(GitOutput {
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                exit,
            }),
            Err(_) => Err(WorktreeError::Timeout {
                cwd: cwd.to_owned(),
                args: owned_args(args),
                timeout: self.timeout,
            }),
        }
    }

    /// Like [`WorktreeManager::run_git`], but a non-zero exit becomes [`WorktreeError::GitFailed`]
    /// and only stdout is returned.
    async fn run_git_ok(&self, cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
        let output = self.run_git(cwd, args).await?;
        if !output.success() {
            return Err(WorktreeError::GitFailed {
                cwd: cwd.to_owned(),
                args: owned_args(args),
                detail: describe_failure(&output),
            });
        }
        Ok(output.stdout)
    }
}

/// The result of running one git command to completion.
struct GitOutput {
    stdout: String,
    exit: Exit,
}

impl GitOutput {
    fn success(&self) -> bool {
        self.exit.info.success()
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

/// Parses `git diff --name-status`' output: one `STATUS\tpath` line, or `RNNN\told\tnew` for a
/// rename or copy.
fn parse_name_status(output: &str) -> Vec<ChangedFile> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            let code = fields.next().unwrap_or_default();
            let first = fields.next().unwrap_or_default().to_owned();
            let second = fields.next().map(str::to_owned);
            let status = match code.as_bytes().first() {
                Some(b'A') => ChangeStatus::Added,
                Some(b'M') => ChangeStatus::Modified,
                Some(b'D') => ChangeStatus::Deleted,
                Some(b'R') => ChangeStatus::Renamed,
                Some(b'C') => ChangeStatus::Copied,
                Some(b'T') => ChangeStatus::TypeChanged,
                Some(b'U') => ChangeStatus::Unmerged,
                _ => ChangeStatus::Unknown(code.to_owned()),
            };
            match (&status, second) {
                (ChangeStatus::Renamed | ChangeStatus::Copied, Some(new_path)) => ChangedFile {
                    status,
                    path: new_path,
                    old_path: Some(first),
                },
                _ => ChangedFile {
                    status,
                    path: first,
                    old_path: None,
                },
            }
        })
        .collect()
}

/// Cuts `text` to at most `max_bytes`, on a UTF-8 boundary, and notes when it did.
fn cap_diff(text: String, max_bytes: usize) -> Diff {
    if text.len() <= max_bytes {
        return Diff {
            text,
            truncated: false,
        };
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Diff {
        text: format!(
            "{}\n... (diff truncated at {max_bytes} bytes)\n",
            &text[..end]
        ),
        truncated: true,
    }
}

/// The paths of `dir`'s entries, or the error reading it.
async fn read_dir_entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    let mut entries = Vec::new();
    while let Some(entry) = read_dir.next_entry().await? {
        entries.push(entry.path());
    }
    Ok(entries)
}
