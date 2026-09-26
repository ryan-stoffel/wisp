//! Reviewing and accepting a run's commit (#157, #68).
//!
//! [`WorktreeManager::diff_commits`] and [`WorktreeManager::read_blob`] compare and read two
//! commits: the worktree's base and the run's latest commit. They only read git objects, through
//! the pinned, hardened worktree calls (#166), and never the worktree's files, which the worker
//! owns: a symlink is a blob holding its target, and is never followed.
//!
//! [`WorktreeManager::accept`] merges the run's commit into the project repository's current
//! branch, in the user's own checkout. It works out the result first, in the object store (the
//! commit itself for a fast-forward, or a merge commit from `git merge-tree --write-tree`), then
//! refuses if the merge conflicts, if uncommitted changes touch a file the result changes, or if
//! any file, ignored ones included, sits where the result adds one. Only then does it move the
//! branch and working tree, with `git merge --ff-only --no-overwrite-ignore <result>`, whose
//! two-way checkout keeps every other uncommitted change. If that checkout stops part way (one of
//! the user's own filters failed, or the branch couldn't move), wispd puts back only the files
//! that still hold exactly what git wrote, and names any that changed meanwhile; after a lock
//! failure or a timeout it puts back nothing (see `WorktreeManager::roll_back`). It never pushes.
//!
//! Those calls use the user's own configuration, since the checkout is theirs: global config,
//! filters such as Git LFS's, merge drivers, and identity. Hooks are the exception: like every git
//! call wispd makes, they run with `core.hooksPath=/dev/null` (see `WorktreeManager::run_git`).
//! The merge commit is not signed, because signing can wait on a prompt.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tokio::time::timeout;
use tracing::warn;

use super::{
    ChangeStatus, ChangedFile, DiffStat, NO_DIFF_DRIVERS, WorktreeError, WorktreeManager,
    changed_file, collect, describe_failure, owned_args,
};
use crate::backend::process::Output;

/// The largest file [`WorktreeManager::read_blob`] returns: 4 MiB, which base64 turns into about
/// 5.4 MiB, inside 0007's 8 MiB frame.
pub const MAX_BLOB_BYTES: u64 = 4 * 1024 * 1024;

/// The most unified diff one file carries in [`WorktreeManager::diff_commits`].
const MAX_FILE_DIFF_BYTES: usize = 256 * 1024;

/// The most unified diff [`WorktreeManager::diff_commits`] returns in all, half of 0007's frame.
const MAX_TOTAL_DIFF_BYTES: usize = 4 * 1024 * 1024;

/// The most JSON the file list of [`WorktreeManager::diff_commits`] takes, not counting diffs:
/// paths can be up to 4 KiB each, and escaping can grow them.
const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;

/// About how much JSON one listed file takes besides its paths and diff: field names, stats.
const PER_FILE_OVERHEAD: usize = 160;

/// The most files [`WorktreeManager::diff_commits`] lists. Totals still count every file.
const MAX_DIFF_FILES: usize = 3000;

/// How long Accept's `merge --ff-only` may run: it checks files out through the user's own
/// filters, and a Git LFS smudge can download for a while.
pub(super) const MERGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The longest path `agent/file` takes, in bytes.
const MAX_PATH_BYTES: usize = 4096;

const DIFF_ARGS: &[&str] = &["diff", "--no-color", "--find-renames"];

/// Checks that `path` names a file inside a repository and nothing else: relative, with no
/// empty, `.`, or `..` component, no backslash or NUL, and nothing under `.git` in any case.
///
/// # Errors
///
/// A description of what is wrong, for people.
pub fn validate_repo_path(path: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("path {path:?} {why}"));
    if path.is_empty() {
        return Err("path must not be empty".to_owned());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(format!("path must be at most {MAX_PATH_BYTES} bytes"));
    }
    if path.contains('\0') || path.contains('\\') {
        return refuse("must not contain NUL or a backslash");
    }
    if path.starts_with('/') {
        return refuse("must be relative to the repository root");
    }
    for component in path.split('/') {
        match component {
            "" | "." | ".." => {
                return refuse("must not have an empty, \".\", or \"..\" component");
            }
            name if name.eq_ignore_ascii_case(".git") => {
                return refuse("must not be inside .git");
            }
            _ => {}
        }
    }
    Ok(())
}

/// One file that differs between two commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// How it changed.
    pub status: ChangeStatus,
    /// Its path on the head side, or on the base side for a deletion.
    pub path: String,
    /// Its base-side path, for a rename or a copy.
    pub old_path: Option<String>,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// Whether git treats it as binary.
    pub binary: bool,
    /// Its unified diff, unless it is binary or the total cap was reached first.
    pub diff: Option<String>,
    /// Whether `diff` was cut at the per-file cap.
    pub diff_truncated: bool,
}

/// What differs between two commits, from [`WorktreeManager::diff_commits`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitDiff {
    /// The files, in git's order (by path), at most `MAX_DIFF_FILES`.
    pub files: Vec<FileDiff>,
    /// Totals over every file.
    pub stat: DiffStat,
    /// Whether `files` was cut short.
    pub truncated: bool,
}

/// A file read from a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    /// Its size in bytes.
    pub size: u64,
    /// Its exact bytes, or `None` when it is over the caller's cap.
    pub content: Option<Vec<u8>>,
}

/// How [`WorktreeManager::accept`] brought a commit in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeHow {
    /// The branch moved forward to the commit.
    FastForward,
    /// wispd made a merge commit.
    Merge,
    /// The branch already contained the commit.
    UpToDate,
}

/// What [`WorktreeManager::accept`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// The commit the branch points at now.
    pub commit: String,
    /// The branch, such as `main`.
    pub into: String,
    /// How.
    pub how: MergeHow,
}

/// Why [`WorktreeManager::accept`] changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    /// Refused before touching anything; the message says why and what to do.
    #[error("{0}")]
    Refused(String),
    /// The commit conflicts with the branch in these files.
    #[error("the agent's changes conflict with {into} in {}; ask the agent to update its branch, or merge it yourself", list_paths(.paths))]
    Conflict {
        /// The branch.
        into: String,
        /// The conflicting paths.
        paths: Vec<String>,
    },
    /// git failed or could not run.
    #[error(transparent)]
    Git(#[from] WorktreeError),
}

/// The paths for a message: the first ten, then how many more.
fn list_paths<S: AsRef<str>>(paths: &[S]) -> String {
    const SHOWN: usize = 10;
    let mut list = paths
        .iter()
        .take(SHOWN)
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        let _ = write!(list, " and {} more", paths.len() - SHOWN);
    }
    list
}

/// Operations in progress that make a merge unsafe, by the file or folder git keeps for each.
const IN_PROGRESS: &[(&str, &str)] = &[
    ("MERGE_HEAD", "a merge"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase or git am"),
];

impl WorktreeManager {
    /// The files that differ between commits `base` and `head`, with stats and size-capped
    /// unified diffs, read through the worktree's pinned git folder like
    /// [`WorktreeManager::diff`] (#166). It reads only commits, never the worktree's files.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn diff_commits(
        &self,
        worktree_path: &Path,
        git_dir: &Path,
        base: &str,
        head: &str,
    ) -> Result<CommitDiff, WorktreeError> {
        if base == head {
            return Ok(CommitDiff::default());
        }
        let with = |extra: &[&'static str]| -> Vec<&str> {
            let mut args: Vec<&str> = DIFF_ARGS.to_vec();
            args.extend_from_slice(NO_DIFF_DRIVERS);
            args.extend_from_slice(extra);
            args.extend_from_slice(&[base, head]);
            args
        };
        let names = self
            .run_worktree_git_ok(worktree_path, git_dir, &with(&["--name-status", "-z"]))
            .await?;
        let entries = parse_name_status_z(&names);
        let numstat = self
            .run_worktree_git_ok(worktree_path, git_dir, &with(&["--numstat", "-z"]))
            .await?;
        let mut counts = parse_numstat_z(&numstat);
        if counts.len() != entries.len() {
            warn!(
                names = entries.len(),
                counts = counts.len(),
                "git's name-status and numstat disagree on a run's files"
            );
            counts = vec![(0, 0, false); entries.len()];
        }
        let stat = DiffStat {
            files: entries.len() as u64,
            insertions: counts.iter().map(|count| count.0).sum(),
            deletions: counts.iter().map(|count| count.1).sum(),
        };
        let mut listing = 0usize;
        let listed = entries
            .iter()
            .take(MAX_DIFF_FILES)
            .take_while(|entry| {
                listing += PER_FILE_OVERHEAD
                    + json_len(&entry.path)
                    + entry.old_path.as_deref().map_or(0, json_len);
                listing <= MAX_LISTING_BYTES
            })
            .count();
        let patches = self
            .patches(worktree_path, git_dir, &with(&["--patch"]), entries.len())
            .await?;
        let files = entries
            .into_iter()
            .zip(counts)
            .take(listed)
            .enumerate()
            .map(|(index, (entry, (insertions, deletions, binary)))| {
                let (diff, diff_truncated) = patches.get(index).cloned().unwrap_or_default();
                FileDiff {
                    status: entry.status,
                    path: entry.path,
                    old_path: entry.old_path,
                    insertions,
                    deletions,
                    binary,
                    diff: if binary { None } else { diff },
                    diff_truncated,
                }
            })
            .collect();
        Ok(CommitDiff {
            files,
            stat,
            truncated: u64::try_from(listed).unwrap_or(u64::MAX) < stat.files,
        })
    }

    /// Streams `git diff --patch` and splits it into one section per file, each capped at
    /// `MAX_FILE_DIFF_BYTES`, keeping sections only until `MAX_TOTAL_DIFF_BYTES` is reached. Both
    /// count the text's size as a JSON string, escapes included, since that is what goes in the
    /// frame: a line of control characters is six times its raw size.
    /// Returns no sections at all when their number isn't `expected`, since they could then not
    /// be matched to files by position.
    async fn patches(
        &self,
        worktree_path: &Path,
        git_dir: &Path,
        args: &[&str],
        expected: usize,
    ) -> Result<Vec<(Option<String>, bool)>, WorktreeError> {
        let spec = self.worktree_spec(worktree_path, git_dir, args).await?;
        let mut process = self.launcher.spawn(&spec)?;
        let read = async {
            let mut sections: Vec<(Option<String>, bool)> = Vec::new();
            // Each section's text, its size as JSON, and whether it was cut short.
            let mut current: Option<(String, usize, bool)> = None;
            let mut total = 0usize;
            let close = |current: &mut Option<(String, usize, bool)>,
                         sections: &mut Vec<(Option<String>, bool)>,
                         total: &mut usize| {
                if let Some((text, size, truncated)) = current.take() {
                    if *total + size <= MAX_TOTAL_DIFF_BYTES {
                        *total += size;
                        sections.push((Some(text), truncated));
                    } else {
                        *total = MAX_TOTAL_DIFF_BYTES;
                        sections.push((None, false));
                    }
                }
            };
            loop {
                match process.next().await {
                    Some(Output::Line(line)) => {
                        if line.starts_with(b"diff --git ") {
                            close(&mut current, &mut sections, &mut total);
                            current = Some((String::new(), 0, false));
                        }
                        if let Some((text, size, truncated)) = &mut current {
                            let line = String::from_utf8_lossy(&line);
                            let line_size = json_len(&line) + 2;
                            if !*truncated && *size + line_size <= MAX_FILE_DIFF_BYTES {
                                text.push_str(&line);
                                text.push('\n');
                                *size += line_size;
                            } else {
                                *truncated = true;
                            }
                        }
                    }
                    Some(Output::Oversized { .. }) => {
                        if let Some((_, _, truncated)) = &mut current {
                            *truncated = true;
                        }
                    }
                    Some(Output::Exited(exit)) => {
                        close(&mut current, &mut sections, &mut total);
                        return Ok((sections, exit));
                    }
                    None => unreachable!("Output::Exited always comes last"),
                }
            }
        };
        let (sections, exit) = match timeout(self.timeout, read).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(WorktreeError::Timeout {
                    cwd: worktree_path.to_owned(),
                    args: owned_args(args),
                    timeout: self.timeout,
                });
            }
        };
        if !exit.info.success() {
            return Err(WorktreeError::GitFailed {
                cwd: worktree_path.to_owned(),
                args: owned_args(args),
                detail: exit.stderr_tail,
            });
        }
        if sections.len() != expected {
            warn!(
                sections = sections.len(),
                expected, "a run's patch doesn't split into one section per file"
            );
            return Ok(Vec::new());
        }
        Ok(sections)
    }

    /// The file at `path` in `commit`, read from git's objects through the worktree's pinned git
    /// folder (#166): `None` when there is no file there (nothing, a folder, or a submodule).
    /// Its content is left out when it is over `max` bytes. `path` must already have passed
    /// [`validate_repo_path`]; it is matched literally, never as a pattern.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`].
    pub async fn read_blob(
        &self,
        worktree_path: &Path,
        git_dir: &Path,
        commit: &str,
        path: &str,
        max: u64,
    ) -> Result<Option<Blob>, WorktreeError> {
        let listing = self
            .run_worktree_git_ok(
                worktree_path,
                git_dir,
                &[
                    "--literal-pathspecs",
                    "ls-tree",
                    "-l",
                    "-z",
                    "--full-tree",
                    commit,
                    "--",
                    path,
                ],
            )
            .await?;
        let Some((object, size)) = listing.split('\0').find_map(|entry| {
            let (meta, name) = entry.split_once('\t')?;
            let mut fields = meta.split_whitespace();
            let (_mode, kind, object, size) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (name == path && kind == "blob").then(|| (object.to_owned(), size.parse::<u64>().ok()))
        }) else {
            return Ok(None);
        };
        let Some(size) = size else {
            return Ok(None);
        };
        if size > max {
            return Ok(Some(Blob {
                size,
                content: None,
            }));
        }
        let args = ["cat-file", "blob", object.as_str()];
        let spec = self.worktree_spec(worktree_path, git_dir, &args).await?;
        let process = self.launcher.spawn(&spec)?;
        let Ok((mut bytes, exit)) = timeout(self.timeout, collect(process)).await else {
            return Err(WorktreeError::Timeout {
                cwd: worktree_path.to_owned(),
                args: owned_args(&args),
                timeout: self.timeout,
            });
        };
        if !exit.info.success() {
            return Err(WorktreeError::GitFailed {
                cwd: worktree_path.to_owned(),
                args: owned_args(&args),
                detail: exit.stderr_tail,
            });
        }
        // `collect` ends every line with `\n`, including a last line that had none.
        let size_bytes = usize::try_from(size).unwrap_or(usize::MAX);
        if bytes.len() == size_bytes + 1 && bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        if bytes.len() != size_bytes {
            return Err(WorktreeError::GitFailed {
                cwd: worktree_path.to_owned(),
                args: owned_args(&args),
                detail: format!("read {} bytes of a {size}-byte file", bytes.len()),
            });
        }
        Ok(Some(Blob {
            size,
            content: Some(bytes),
        }))
    }

    /// Merges `commit` into the current branch of `repo_path`'s checkout: see the module
    /// documentation. `message` is the merge commit's message, when one is needed.
    ///
    /// # Errors
    ///
    /// [`AcceptError::Refused`] or [`AcceptError::Conflict`] with nothing changed, or
    /// [`AcceptError::Git`] if git failed.
    pub async fn accept(
        &self,
        repo_path: &Path,
        commit: &str,
        message: &str,
    ) -> Result<Accepted, AcceptError> {
        let repo_root = self.repo_root(repo_path).await?;
        let _guard = self.lock_repo(&repo_root).await;
        let repo = repo_root.display().to_string();

        let head_ref = self
            .run_checkout_git(&repo_root, &["symbolic-ref", "--quiet", "HEAD"])
            .await?;
        let into = match head_ref.stdout.trim().strip_prefix("refs/heads/") {
            Some(branch) if head_ref.success() => branch.to_owned(),
            _ => {
                return Err(AcceptError::Refused(format!(
                    "{repo} has no branch checked out (its HEAD is detached); check out the \
                     branch to merge into, then accept again"
                )));
            }
        };
        self.refuse_in_progress(&repo_root, &repo).await?;

        let target = self.resolve_commit(&repo_root, commit).await?;
        let head = self.resolve_commit(&repo_root, "HEAD").await?;
        if self.is_ancestor(&repo_root, &target, &head).await? {
            return Ok(Accepted {
                commit: head,
                into,
                how: MergeHow::UpToDate,
            });
        }
        let (result, how) = if self.is_ancestor(&repo_root, &head, &target).await? {
            (target, MergeHow::FastForward)
        } else {
            let merge = self
                .merge_commit(&repo_root, &head, &target, &into, message)
                .await?;
            (merge, MergeHow::Merge)
        };

        let changes = self
            .run_checkout_git_ok(
                &repo_root,
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    "--no-renames",
                    &head,
                    &result,
                ],
            )
            .await?;
        let changes = parse_changes(&changes);
        self.refuse_overlap(&repo_root, &repo, &changes).await?;

        let target = Target {
            repo_root: &repo_root,
            repo: &repo,
            into: &into,
            head: &head,
            result: &result,
            changes: &changes,
        };
        self.check_out(&target).await?;
        Ok(Accepted {
            commit: result,
            into,
            how,
        })
    }

    /// Moves the branch and working tree to the merge's result with `git merge --ff-only`, and
    /// puts back what a failed checkout wrote (see [`Self::roll_back`]).
    pub(super) async fn check_out(&self, target: &Target<'_>) -> Result<(), AcceptError> {
        let Target {
            repo_root,
            repo,
            into,
            result,
            changes,
            ..
        } = *target;
        let merged = self
            .run_git_for(
                repo_root,
                &[
                    "merge",
                    "--ff-only",
                    "--no-autostash",
                    "--no-stat",
                    "--no-overwrite-ignore",
                    result,
                ],
                self.merge_timeout,
            )
            .await;
        let failure = match merged {
            Ok(output) if output.success() => None,
            Ok(output) => {
                let detail = describe_failure(&output);
                // git refused before writing anything: something in the way, or another git
                // process holding the index. Nothing to put back, and a "roll back" here could
                // only overwrite what that other process is writing.
                if detail.contains("would be overwritten") || detail.contains("index.lock") {
                    return Err(AcceptError::Refused(format!(
                        "git could not update {into} in {repo}, so it changed nothing: {detail}"
                    )));
                }
                Some(detail)
            }
            Err(WorktreeError::Timeout { .. }) => {
                let lock = self.git_path(repo_root, "index.lock").await?;
                if tokio::fs::symlink_metadata(&lock).await.is_ok() {
                    let paths: Vec<&str> = changes.iter().map(|(_, path)| path.as_str()).collect();
                    return Err(AcceptError::Refused(format!(
                        "git did not finish updating {into} in {repo} within {}s, and was \
                         stopped. It left {} behind, so wispd put nothing back: these files may \
                         be partly updated: {}. If no other git is running there, remove the \
                         lock, then check git status",
                        self.merge_timeout.as_secs(),
                        lock.display(),
                        list_paths(&paths)
                    )));
                }
                Some(format!(
                    "it did not finish within {}s",
                    self.merge_timeout.as_secs()
                ))
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(failure) = failure {
            if self.resolve_commit(repo_root, "HEAD").await? == *result {
                warn!(repo, %failure, "git reported a failure after moving the branch; the accept stands");
            } else {
                return Err(self.roll_back(target, &failure).await);
            }
        }
        Ok(())
    }

    /// Refuses when a local change would be lost or overwritten by the merge: uncommitted changes
    /// (staged, unstaged, or untracked) to a path it changes, or any file, ignored or not, where it
    /// adds one. `--no-overwrite-ignore` makes git refuse that last case too; this names the files.
    async fn refuse_overlap(
        &self,
        repo_root: &Path,
        repo: &str,
        changes: &[(char, String)],
    ) -> Result<(), AcceptError> {
        let paths: HashSet<&str> = changes.iter().map(|(_, path)| path.as_str()).collect();
        let status = self
            .run_checkout_git_ok(
                repo_root,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--no-renames",
                ],
            )
            .await?;
        let mut overlap: Vec<&str> = z_tokens(&status)
            .filter_map(|entry| entry.get(3..))
            .filter(|path| paths.contains(path))
            .collect();
        for (kind, path) in changes {
            if *kind == 'A'
                && tokio::fs::symlink_metadata(repo_root.join(path))
                    .await
                    .is_ok()
            {
                overlap.push(path);
            }
        }
        if overlap.is_empty() {
            return Ok(());
        }
        overlap.sort_unstable();
        overlap.dedup();
        Err(AcceptError::Refused(format!(
            "uncommitted or ignored files in {repo} are in the way of files the agent changed: {}; \
             commit, stash, or move them, then accept again",
            list_paths(&overlap)
        )))
    }

    /// Puts back the files a failed `merge --ff-only` wrote before it stopped, such as when one
    /// of the user's own filters (a Git LFS smudge, say) failed part way. Only a file that still
    /// holds exactly the merge's version is put back, index and working tree, from HEAD (a file
    /// the merge added is removed): nothing else can have written it since. A file that holds
    /// HEAD's version needs nothing. A file that holds neither was written by someone else during
    /// the checkout, so it is left alone and named. `.gitattributes` files go first, so the rest
    /// are checked out with the user's own attributes again.
    async fn roll_back(&self, target: &Target<'_>, failure: &str) -> AcceptError {
        let Target {
            repo_root,
            repo,
            into,
            head,
            result,
            changes,
        } = *target;
        let paths: Vec<&str> = changes.iter().map(|(_, path)| path.as_str()).collect();
        let states = async {
            let at_head = self.blob_ids(repo_root, head, &paths).await?;
            let at_result = self.blob_ids(repo_root, result, &paths).await?;
            let on_disk = self.worktree_ids(repo_root, &paths).await?;
            Ok::<_, WorktreeError>((at_head, at_result, on_disk))
        }
        .await;
        let (at_head, at_result, on_disk) = match states {
            Ok(states) => states,
            Err(error) => {
                return AcceptError::Refused(format!(
                    "git could not update {into} in {repo} ({failure}), and wispd could not tell \
                     which files it had written ({error}), so it put nothing back; check git \
                     status for {}",
                    list_paths(&paths)
                ));
            }
        };
        let mut restore: Vec<&str> = Vec::new();
        let mut remove: Vec<&str> = Vec::new();
        let mut foreign: Vec<&str> = Vec::new();
        for path in &paths {
            let (theirs, ours, now) =
                (at_result.get(*path), at_head.get(*path), on_disk.get(*path));
            if now == theirs && theirs != ours {
                if ours.is_none() {
                    remove.push(path);
                } else {
                    restore.push(path);
                }
            } else if now != ours {
                foreign.push(path);
            }
        }
        let left = self.put_back(repo_root, head, &mut restore, &remove).await;
        let mut message = format!("git could not update {into} in {repo} ({failure}). ");
        if left.is_empty() && foreign.is_empty() {
            message
                .push_str("wispd put back the files it had started to write, so nothing changed");
        } else {
            message.push_str("wispd put back the files only git had written");
            if !foreign.is_empty() {
                let _ = write!(
                    message,
                    "; it left alone {}, which changed while git ran",
                    list_paths(&foreign)
                );
            }
            if !left.is_empty() {
                let _ = write!(message, "; it could not put back {}", list_paths(&left));
            }
            message.push_str(". Check them with git status");
        }
        AcceptError::Refused(message)
    }

    /// Removes `remove` from the index and working tree, and restores `restore` in both from
    /// `head`, `.gitattributes` files first. Returns the paths it could not put back.
    async fn put_back(
        &self,
        repo_root: &Path,
        head: &str,
        restore: &mut [&str],
        remove: &[&str],
    ) -> Vec<String> {
        restore.sort_by_key(|path| !is_attributes(path));
        let mut left: Vec<String> = Vec::new();
        for path in remove
            .iter()
            .copied()
            .filter(|path| is_attributes(path))
            .chain(remove.iter().copied().filter(|path| !is_attributes(path)))
        {
            let ok = self
                .run_git(
                    repo_root,
                    &[
                        "--literal-pathspecs",
                        "rm",
                        "--quiet",
                        "--cached",
                        "--ignore-unmatch",
                        "--",
                        path,
                    ],
                )
                .await
                .is_ok_and(|output| output.success());
            match tokio::fs::remove_file(repo_root.join(path)).await {
                Ok(()) if ok => remove_empty_parents(repo_root, path).await,
                Err(error) if ok && error.kind() == std::io::ErrorKind::NotFound => {}
                _ => left.push(path.to_owned()),
            }
        }
        let (attributes, rest): (Vec<&str>, Vec<&str>) =
            restore.iter().partition(|path| is_attributes(path));
        for chunk in attributes.chunks(1).chain(rest.chunks(200)) {
            let mut args = vec![
                "--literal-pathspecs",
                "restore",
                "--source",
                head,
                "--staged",
                "--worktree",
                "--",
            ];
            args.extend_from_slice(chunk);
            if !self
                .run_git(repo_root, &args)
                .await
                .is_ok_and(|output| output.success())
            {
                left.extend(chunk.iter().map(|path| (*path).to_owned()));
            }
        }
        left
    }

    /// The blob id of each of `paths` in `commit`, by path; a path that isn't a file there is
    /// missing from the map.
    async fn blob_ids(
        &self,
        repo_root: &Path,
        commit: &str,
        paths: &[&str],
    ) -> Result<HashMap<String, String>, WorktreeError> {
        let mut ids = HashMap::new();
        for chunk in paths.chunks(200) {
            let mut args = vec![
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--full-tree",
                commit,
                "--",
            ];
            args.extend_from_slice(chunk);
            let listing = self.run_checkout_git_ok(repo_root, &args).await?;
            for entry in z_tokens(&listing) {
                let Some((meta, name)) = entry.split_once('\t') else {
                    continue;
                };
                let mut fields = meta.split_whitespace();
                if let (Some(mode), Some(_), Some(object)) =
                    (fields.next(), fields.next(), fields.next())
                {
                    ids.insert(name.to_owned(), format!("{mode} {object}"));
                }
            }
        }
        Ok(ids)
    }

    /// What each of `paths` holds in the working tree, as the id git would give it (through the
    /// same filters `git add` would use), keyed like [`Self::blob_ids`]; a missing path is
    /// missing from the map. A symlink or folder gets an id no blob matches, so it counts as
    /// written by someone else unless absent on both sides.
    async fn worktree_ids(
        &self,
        repo_root: &Path,
        paths: &[&str],
    ) -> Result<HashMap<String, String>, WorktreeError> {
        let mut ids = HashMap::new();
        let mut files: Vec<(&str, &str)> = Vec::new();
        for path in paths {
            match tokio::fs::symlink_metadata(repo_root.join(path)).await {
                Ok(meta) if meta.is_file() => {
                    let mode = if std::os::unix::fs::PermissionsExt::mode(&meta.permissions())
                        & 0o111
                        != 0
                    {
                        "100755"
                    } else {
                        "100644"
                    };
                    files.push((path, mode));
                }
                Ok(_) => {
                    ids.insert((*path).to_owned(), "other".to_owned());
                }
                Err(_) => {}
            }
        }
        for chunk in files.chunks(200) {
            let mut args = vec!["hash-object", "--"];
            args.extend(chunk.iter().map(|(path, _)| *path));
            let hashes = self.run_checkout_git_ok(repo_root, &args).await?;
            for ((path, mode), object) in chunk.iter().zip(hashes.lines()) {
                ids.insert((*path).to_owned(), format!("{mode} {}", object.trim()));
            }
        }
        Ok(ids)
    }

    /// `git rev-parse --git-path <name>` as an absolute path.
    async fn git_path(&self, repo_root: &Path, name: &str) -> Result<PathBuf, WorktreeError> {
        let path = self
            .run_checkout_git_ok(
                repo_root,
                &["rev-parse", "--path-format=absolute", "--git-path", name],
            )
            .await?;
        Ok(PathBuf::from(path.trim()))
    }

    async fn refuse_in_progress(&self, repo_root: &Path, repo: &str) -> Result<(), AcceptError> {
        let mut args = vec!["rev-parse", "--path-format=absolute"];
        for (name, _) in IN_PROGRESS {
            args.extend_from_slice(&["--git-path", name]);
        }
        let paths = self.run_checkout_git_ok(repo_root, &args).await?;
        for ((_, what), path) in IN_PROGRESS.iter().zip(paths.lines()) {
            if tokio::fs::symlink_metadata(path).await.is_ok() {
                return Err(AcceptError::Refused(format!(
                    "{repo} is in the middle of {what}; finish or abort it, then accept again"
                )));
            }
        }
        Ok(())
    }

    async fn is_ancestor(
        &self,
        repo_root: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, WorktreeError> {
        let output = self
            .run_checkout_git(
                repo_root,
                &["merge-base", "--is-ancestor", ancestor, descendant],
            )
            .await?;
        match output.exit.info.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(WorktreeError::GitFailed {
                cwd: repo_root.to_owned(),
                args: owned_args(&["merge-base", "--is-ancestor", ancestor, descendant]),
                detail: describe_failure(&output),
            }),
        }
    }

    /// A merge commit of `head` and `target`, made in the object store only, or the paths that
    /// conflict.
    async fn merge_commit(
        &self,
        repo_root: &Path,
        head: &str,
        target: &str,
        into: &str,
        message: &str,
    ) -> Result<String, AcceptError> {
        let tree = self
            .run_checkout_git(
                repo_root,
                &[
                    "merge-tree",
                    "--write-tree",
                    "--name-only",
                    "--no-messages",
                    "-z",
                    head,
                    target,
                ],
            )
            .await?;
        match tree.exit.info.code {
            Some(0) => {}
            Some(1) => {
                let mut paths: Vec<String> =
                    z_tokens(&tree.stdout).skip(1).map(str::to_owned).collect();
                paths.dedup();
                return Err(AcceptError::Conflict {
                    into: into.to_owned(),
                    paths,
                });
            }
            _ => {
                return Err(AcceptError::Git(WorktreeError::GitFailed {
                    cwd: repo_root.to_owned(),
                    args: owned_args(&["merge-tree", "--write-tree", head, target]),
                    detail: describe_failure(&tree),
                }));
            }
        }
        let tree = z_tokens(&tree.stdout)
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if self.resolve_identity(repo_root).await?.is_none() {
            return Err(AcceptError::Refused(format!(
                "{} has no git identity configured (user.name and user.email), which the merge \
                 commit needs; set one, then accept again",
                repo_root.display()
            )));
        }
        let commit = self
            .run_checkout_git_ok(
                repo_root,
                &[
                    "commit-tree",
                    "--no-gpg-sign",
                    &tree,
                    "-p",
                    head,
                    "-p",
                    target,
                    "-m",
                    message,
                ],
            )
            .await?;
        Ok(commit.trim().to_owned())
    }

    /// `git args` in the user's own checkout, with its own configuration. Like every call through
    /// [`WorktreeManager::run_git`], it runs no hooks.
    async fn run_checkout_git(
        &self,
        repo_root: &Path,
        args: &[&str],
    ) -> Result<super::GitOutput, WorktreeError> {
        self.run_git(repo_root, args).await
    }

    async fn run_checkout_git_ok(
        &self,
        repo_root: &Path,
        args: &[&str],
    ) -> Result<String, WorktreeError> {
        let output = self.run_checkout_git(repo_root, args).await?;
        if !output.success() {
            return Err(WorktreeError::GitFailed {
                cwd: repo_root.to_owned(),
                args: owned_args(args),
                detail: describe_failure(&output),
            });
        }
        Ok(output.stdout)
    }
}

/// Parses `git diff --name-status -z`: a status, then one path, or two for a rename or copy,
/// each its own NUL-terminated token, so a path may hold a tab or a newline.
fn parse_name_status_z(output: &str) -> Vec<ChangedFile> {
    let mut tokens = z_tokens(output);
    let mut files = Vec::new();
    while let Some(code) = tokens.next() {
        let first = tokens.next().unwrap_or_default().to_owned();
        let second = (code.starts_with('R') || code.starts_with('C'))
            .then(|| tokens.next().unwrap_or_default().to_owned());
        files.push(changed_file(code, first, second));
    }
    files
}

/// The size of `text` as a JSON string's content, as `serde_json` writes it: `"` and `\\` and the
/// short escapes (`\n`, `\t`, ...) take two bytes, other control characters six (`\u00XX`).
fn json_len(text: &str) -> usize {
    text.bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum()
}

/// Parses `git diff --name-status -z --no-renames`: `(status letter, path)` per file.
pub(super) fn parse_changes(output: &str) -> Vec<(char, String)> {
    let mut tokens = z_tokens(output);
    let mut changes = Vec::new();
    while let (Some(code), Some(path)) = (tokens.next(), tokens.next()) {
        changes.push((code.chars().next().unwrap_or('M'), path.to_owned()));
    }
    changes
}

/// Where Accept's checkout is going: see [`WorktreeManager::check_out`].
#[derive(Clone, Copy)]
pub(super) struct Target<'a> {
    pub(super) repo_root: &'a Path,
    pub(super) repo: &'a str,
    pub(super) into: &'a str,
    pub(super) head: &'a str,
    pub(super) result: &'a str,
    pub(super) changes: &'a [(char, String)],
}

/// Removes the folders above `path` that are left empty, up to `repo_root`.
async fn remove_empty_parents(repo_root: &Path, path: &str) {
    let mut dir = Path::new(path).parent();
    while let Some(parent) = dir.filter(|parent| !parent.as_os_str().is_empty()) {
        if tokio::fs::remove_dir(repo_root.join(parent)).await.is_err() {
            break;
        }
        dir = parent.parent();
    }
}

fn is_attributes(path: &str) -> bool {
    path == ".gitattributes" || path.ends_with("/.gitattributes")
}

/// The NUL-terminated tokens of a `-z` output. The output collector ends the last line with a
/// newline git never wrote, which is dropped here.
fn z_tokens(output: &str) -> impl Iterator<Item = &str> {
    let output = output.strip_suffix('\n').unwrap_or(output);
    output.split('\0').filter(|token| !token.is_empty())
}

/// Parses `git diff --numstat -z` into `(insertions, deletions, binary)` per file, in order. A
/// rename's entry is `added\tdeleted\t` followed by its two paths as separate tokens.
fn parse_numstat_z(output: &str) -> Vec<(u64, u64, bool)> {
    let mut tokens = z_tokens(output);
    let mut counts = Vec::new();
    while let Some(token) = tokens.next() {
        let mut fields = token.splitn(3, '\t');
        let (Some(added), Some(deleted), path) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if path.unwrap_or_default().is_empty() {
            tokens.next();
            tokens.next();
        }
        let binary = added == "-" && deleted == "-";
        counts.push((
            added.parse().unwrap_or(0),
            deleted.parse().unwrap_or(0),
            binary,
        ));
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::{json_len, parse_name_status_z, parse_numstat_z, validate_repo_path};
    use crate::worktree::ChangeStatus;

    #[test]
    fn only_plain_relative_paths_outside_git_are_valid() {
        for good in [
            "README.md",
            "src/a b/c.ts",
            "a/.gitignore",
            "..rc",
            "dir/...x",
        ] {
            assert_eq!(validate_repo_path(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../outside",
            "a/../../b",
            "a/./b",
            "./a",
            "a//b",
            "a/",
            ".git/config",
            "sub/.GIT/hooks/x",
            ".Git",
            "a\\..\\b",
            "a\0b",
        ] {
            assert!(validate_repo_path(bad).is_err(), "{bad:?}");
        }
        assert!(validate_repo_path(&"a".repeat(4097)).is_err());
    }

    #[test]
    fn json_len_counts_escapes_as_serde_json_writes_them() {
        for text in [
            "plain",
            "quote \" and \\",
            "tab\tnew\nline\r",
            "\u{1}\u{1f}\u{7f}",
            "é✓",
            "",
        ] {
            let encoded = serde_json::to_string(text).unwrap();
            assert_eq!(json_len(text), encoded.len() - 2, "{text:?}");
        }
    }

    #[test]
    fn z_outputs_parse_renames_and_binaries_in_order() {
        let names = "M\0src/a.rs\0R087\0old name.rs\0new\tname.rs\0A\0img.png\0\n";
        let files = parse_name_status_z(names);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].status, ChangeStatus::Modified);
        assert_eq!(files[1].status, ChangeStatus::Renamed);
        assert_eq!(files[1].path, "new\tname.rs");
        assert_eq!(files[1].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(files[2].path, "img.png");
        let numstat = "3\t1\tsrc/a.rs\x002\t2\t\0old name.rs\0new\tname.rs\0-\t-\timg.png\0\n";
        assert_eq!(
            parse_numstat_z(numstat),
            [(3, 1, false), (2, 2, false), (0, 0, true)]
        );
    }
}
