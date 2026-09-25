//! Tests against real, temporary git repositories and a real `git` binary, in the same style as
//! `backend::process`'s own tests spawning real processes.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use wisp_protocol::RunId;

use super::{ChangeStatus, WorktreeError, WorktreeManager};
use crate::backend::process::{Environment, Launcher};
use crate::paths::DataDir;

fn manager(data_root: &Path) -> WorktreeManager {
    let launcher = Launcher::new(DataDir::new(data_root).unwrap(), Environment::inherited());
    WorktreeManager::new(launcher, data_root)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .expect("git should run");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git -C {} {args:?}: {output:?}",
        dir.display()
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn rev_parse(dir: &Path, reference: &str) -> String {
    git_output(dir, &["rev-parse", reference])
}

/// Like [`git_output`], but with the git directory pinned explicitly instead of discovered from
/// `work_tree`'s `.git` file: used to check on a repository after a test has rewritten that file,
/// once `work_tree` itself can no longer be trusted to find it.
fn git_output_pinned(work_tree: &Path, git_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .arg(format!("--work-tree={}", work_tree.display()))
        .args(args)
        .output()
        .expect("git should run");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Writes an executable shell script at `path`, creating its parent folder if needed, that
/// touches `sentinel` when run. Every hook, hooksPath, `.gitattributes` driver, and filter test
/// below plants one of these and then asserts `sentinel` was never created, proving wispd's
/// worktree-scoped git calls never ran it (#166).
fn write_sentinel_script(path: &Path, sentinel: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, format!("#!/bin/sh\ntouch '{}'\n", sentinel.display())).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn worktree_count(repo: &Path) -> usize {
    let output = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "list",
            "--porcelain",
        ])
        .output()
        .expect("git should run");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

/// Initializes a repo at `dir` with one commit and a local identity, and returns `dir`.
fn init_repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
    dir.to_owned()
}

#[tokio::test]
async fn create_makes_a_worktree_on_a_new_branch_from_head() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    assert!(created.path.starts_with(mgr.root()));
    assert!(created.path.is_dir(), "{created:?}");
    assert!(created.branch.starts_with("wisp/"), "{created:?}");
    assert_eq!(
        git_output(&created.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        created.branch
    );
    assert_eq!(created.base, rev_parse(&repo, "HEAD"));
    assert_eq!(
        worktree_count(&repo),
        2,
        "the main worktree plus the new one"
    );
}

#[tokio::test]
async fn create_refuses_a_missing_repo() {
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let missing = data_dir.path().join("does-not-exist");

    let error = mgr
        .create(&missing, RunId::generate(), None)
        .await
        .unwrap_err();

    assert!(
        matches!(error, WorktreeError::NotAGitRepo { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn create_refuses_a_directory_that_is_not_a_repo() {
    let not_repo = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let error = mgr
        .create(not_repo.path(), RunId::generate(), None)
        .await
        .unwrap_err();

    assert!(
        matches!(error, WorktreeError::NotAGitRepo { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn create_works_from_a_detached_head() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let head = rev_parse(&repo, "HEAD");
    git(&repo, &["checkout", "-q", &head]);
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    assert_eq!(created.base, head);
}

#[tokio::test]
async fn create_refuses_a_dirty_default_base() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    std::fs::write(repo.join("README.md"), "changed\n").unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let error = mgr
        .create(&repo, RunId::generate(), None)
        .await
        .unwrap_err();

    assert!(
        matches!(error, WorktreeError::DirtyBase { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn create_with_an_explicit_base_skips_the_dirty_check() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let head = rev_parse(&repo, "HEAD");
    std::fs::write(repo.join("README.md"), "changed\n").unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let created = mgr
        .create(&repo, RunId::generate(), Some(&head))
        .await
        .unwrap();

    assert_eq!(created.base, head);
}

#[tokio::test]
async fn create_refuses_a_base_that_does_not_resolve() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let error = mgr
        .create(&repo, RunId::generate(), Some("does-not-exist"))
        .await
        .unwrap_err();

    assert!(
        matches!(error, WorktreeError::UnknownRevision { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn concurrent_creates_on_one_repo_both_succeed() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());

    let (first, second) = tokio::join!(
        mgr.create(&repo, RunId::generate(), None),
        mgr.create(&repo, RunId::generate(), None)
    );
    let first = first.unwrap();
    let second = second.unwrap();

    assert_ne!(first.path, second.path);
    assert_ne!(first.branch, second.branch);
    assert!(first.path.is_dir());
    assert!(second.path.is_dir());
    assert_eq!(worktree_count(&repo), 3, "main plus the two new worktrees");
}

#[tokio::test]
async fn changed_files_and_diff_see_uncommitted_and_committed_changes_the_same_way() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    std::fs::write(created.path.join("README.md"), "edited\n").unwrap();
    std::fs::write(created.path.join("new.txt"), "new file\n").unwrap();

    let mut changed = mgr
        .changed_files(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(changed.len(), 2, "{changed:?}");
    assert_eq!(changed[0].path, "README.md");
    assert_eq!(changed[0].status, ChangeStatus::Modified);
    assert_eq!(changed[1].path, "new.txt");
    assert_eq!(changed[1].status, ChangeStatus::Added);
    assert!(changed[1].old_path.is_none());

    let diff = mgr
        .diff(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();
    assert!(!diff.truncated);
    assert!(diff.text.contains("new.txt"), "{}", diff.text);

    let commit = mgr
        .commit_all(&created.path, &created.git_dir, "agent changes")
        .await
        .unwrap()
        .expect("there was something to commit");
    assert_eq!(commit.sha, rev_parse(&created.path, "HEAD"));

    // The same base comparison sees the same changes, now committed.
    let changed_after = mgr
        .changed_files(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();
    assert_eq!(changed_after.len(), 2, "{changed_after:?}");
}

#[tokio::test]
async fn diff_is_truncated_past_the_cap() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path()).with_max_diff_bytes(200);
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();
    std::fs::write(created.path.join("big.txt"), "x".repeat(10_000)).unwrap();

    let diff = mgr
        .diff(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();

    assert!(diff.truncated);
    assert!(diff.text.len() < 10_000, "{}", diff.text.len());
    assert!(diff.text.contains("truncated"));
}

#[tokio::test]
async fn commit_all_is_a_no_op_when_nothing_changed() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    let commit = mgr
        .commit_all(&created.path, &created.git_dir, "nothing to see")
        .await
        .unwrap();

    assert!(commit.is_none());
    assert_eq!(
        rev_parse(&created.path, "HEAD"),
        created.base,
        "no commit should have been made"
    );
}

#[tokio::test]
async fn commit_all_refuses_without_a_configured_identity() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    git(&repo, &["config", "--unset", "user.name"]);
    git(&repo, &["config", "--unset", "user.email"]);

    // A fresh, empty HOME keeps the host's own global (or system) git identity, if any, from
    // leaking into this repo and making the test flaky.
    let data_dir = tempfile::tempdir().unwrap();
    let empty_home = tempfile::tempdir().unwrap();
    let path = std::env::var("PATH").unwrap();
    let base: Environment = [
        ("PATH", path.as_str()),
        ("HOME", empty_home.path().to_str().unwrap()),
    ]
    .into_iter()
    .collect();
    let launcher = Launcher::new(DataDir::new(data_dir.path()).unwrap(), base);
    let mgr = WorktreeManager::new(launcher, data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();
    std::fs::write(created.path.join("README.md"), "edited\n").unwrap();

    let error = mgr
        .commit_all(&created.path, &created.git_dir, "should not commit")
        .await
        .unwrap_err();

    assert!(
        matches!(error, WorktreeError::MissingIdentity { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn remove_deletes_the_worktree_and_its_branch() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();
    assert!(created.path.exists());

    mgr.remove(&repo, &created.path, &created.branch)
        .await
        .unwrap();

    assert!(!created.path.exists());
    assert_eq!(worktree_count(&repo), 1, "only the main worktree remains");
    let branches = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "branch",
            "--list",
            &created.branch,
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8(branches.stdout)
            .unwrap()
            .trim()
            .is_empty()
    );
}

#[tokio::test]
async fn gc_orphans_removes_an_unknown_worktree_and_keeps_a_known_one() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let known = mgr.create(&repo, RunId::generate(), None).await.unwrap();
    let orphan = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    let mut keep = HashSet::new();
    keep.insert(known.path.clone());
    let report = mgr.gc_orphans(&keep).await;

    assert_eq!(
        report.removed.as_slice(),
        std::slice::from_ref(&orphan.path)
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(known.path.exists());
    assert!(!orphan.path.exists());
    assert_eq!(worktree_count(&repo), 2, "main plus the known worktree");
}

#[tokio::test]
async fn gc_orphans_removes_a_folder_that_is_not_a_worktree_at_all() {
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let bogus = mgr.root().join("some-project").join("not-a-worktree");
    std::fs::create_dir_all(&bogus).unwrap();
    std::fs::write(bogus.join("file.txt"), "hi").unwrap();

    let report = mgr.gc_orphans(&HashSet::new()).await;

    assert_eq!(report.removed.as_slice(), std::slice::from_ref(&bogus));
    assert!(!bogus.exists());
    assert!(
        !bogus.parent().unwrap().exists(),
        "the now-empty project folder should be cleaned up too"
    );
}

#[tokio::test]
async fn gc_orphans_on_a_missing_root_does_nothing() {
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(&data_dir.path().join("never-created"));

    let report = mgr.gc_orphans(&HashSet::new()).await;

    assert!(report.removed.is_empty());
    assert!(report.errors.is_empty());
}

// #166: a worker controls every file in its worktree. These tests plant the vectors #137 found
// (a hook, a repo-configured `core.hooksPath` reaching into the worktree, a `.gitattributes`
// diff or filter driver, and a rewritten `.git` file) and check the sentinel each one's script
// would create never appears.

#[tokio::test]
async fn commit_all_does_not_run_a_post_commit_hook_via_a_repo_configured_hooks_path() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    // As if the repository already had husky-style hooks configured: `core.hooksPath` points at
    // a folder inside the worktree, which a worker can write. `--no-verify` alone would not stop
    // this: it only skips `pre-commit` and `commit-msg`, not `post-commit`.
    let hooks_dir = created.path.join(".husky").join("_");
    let sentinel = data_dir.path().join("sentinel-hooks-path");
    write_sentinel_script(&hooks_dir.join("post-commit"), &sentinel);
    git(
        &created.path,
        &["config", "core.hooksPath", hooks_dir.to_str().unwrap()],
    );

    std::fs::write(created.path.join("README.md"), "edited\n").unwrap();
    let commit = mgr
        .commit_all(&created.path, &created.git_dir, "agent changes")
        .await
        .unwrap()
        .expect("there was something to commit");

    assert_eq!(commit.sha, rev_parse(&created.path, "HEAD"));
    assert!(!sentinel.exists(), "the post-commit hook must not have run");
}

#[tokio::test]
async fn commit_all_does_not_run_a_hook_configured_via_an_included_config_file() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    // As if the repository's own config includes a tracked file for shared settings: the
    // `includeIf` itself is legitimate repo config, but the included file lives in the worktree,
    // where a worker can edit it, and it sets `core.hooksPath` (#137's husky scenario, by way of
    // an include rather than a direct setting).
    let hooks_dir = created.path.join("shared-hooks");
    let sentinel = data_dir.path().join("sentinel-includeif");
    write_sentinel_script(&hooks_dir.join("post-commit"), &sentinel);
    let included_config = created.path.join(".gitconfig-shared");
    std::fs::write(
        &included_config,
        format!("[core]\n\thooksPath = {}\n", hooks_dir.display()),
    )
    .unwrap();
    git(
        &created.path,
        &[
            "config",
            "includeIf.gitdir:**.path",
            included_config.to_str().unwrap(),
        ],
    );
    assert_eq!(
        git_output(&created.path, &["config", "--get", "core.hooksPath"]),
        hooks_dir.to_str().unwrap(),
        "the include must have applied core.hooksPath, or this test proves nothing"
    );

    std::fs::write(created.path.join("README.md"), "edited\n").unwrap();
    let commit = mgr
        .commit_all(&created.path, &created.git_dir, "agent changes")
        .await
        .unwrap()
        .expect("there was something to commit");

    assert_eq!(commit.sha, rev_parse(&created.path, "HEAD"));
    assert!(
        !sentinel.exists(),
        "a hook from an included config file must not have run"
    );
}

#[tokio::test]
async fn diff_does_not_run_a_gitattributes_textconv_driver() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    // The driver itself is configured locally (as a repository's own `.git/config` might be, for
    // example by git-lfs); the worker's own writable surface is `.gitattributes`, routing a file
    // of its choosing through that driver.
    let sentinel = data_dir.path().join("sentinel-textconv");
    let script = created.path.join("textconv.sh");
    write_sentinel_script(&script, &sentinel);
    git(
        &created.path,
        &["config", "diff.evil.textconv", script.to_str().unwrap()],
    );
    std::fs::write(created.path.join(".gitattributes"), "*.bin diff=evil\n").unwrap();
    std::fs::write(created.path.join("data.bin"), "binary-ish\n").unwrap();

    mgr.diff(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();

    assert!(!sentinel.exists(), "the textconv driver must not have run");
}

#[tokio::test]
async fn stage_all_does_not_run_a_gitattributes_filter_from_a_fake_global_config() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();

    // A fake "ambient" HOME with a global gitconfig defining a filter driver, standing in for
    // whatever an inherited or otherwise compromised environment might already have configured.
    // A worker can't write here, but wispd's worktree-scoped calls must not reach it either
    // way — `GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL`, and `HOME` are all overridden.
    let fake_home = tempfile::tempdir().unwrap();
    let sentinel = fake_home.path().join("sentinel-filter");
    let script = fake_home.path().join("evil-clean.sh");
    write_sentinel_script(&script, &sentinel);
    std::fs::write(
        fake_home.path().join(".gitconfig"),
        format!("[filter \"evil\"]\n\tclean = {}\n", script.display()),
    )
    .unwrap();

    let data_dir = tempfile::tempdir().unwrap();
    let path = std::env::var("PATH").unwrap();
    let base: Environment = [
        ("PATH", path.as_str()),
        ("HOME", fake_home.path().to_str().unwrap()),
    ]
    .into_iter()
    .collect();
    let launcher = Launcher::new(DataDir::new(data_dir.path()).unwrap(), base);
    let mgr = WorktreeManager::new(launcher, data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    std::fs::write(
        created.path.join(".gitattributes"),
        "*.secret filter=evil\n",
    )
    .unwrap();
    std::fs::write(created.path.join("leak.secret"), "sensitive\n").unwrap();

    mgr.changed_files(&created.path, &created.git_dir, &created.base)
        .await
        .unwrap();

    assert!(!sentinel.exists(), "the filter driver must not have run");
}

#[tokio::test]
async fn worktree_git_commands_ignore_a_rewritten_git_file() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();

    // A second, fully attacker-controlled repository, with a hook that would prove it ran.
    let evil_dir = tempfile::tempdir().unwrap();
    let evil_repo = init_repo(evil_dir.path()).canonicalize().unwrap();
    let sentinel = data_dir.path().join("sentinel-git-file-redirect");
    write_sentinel_script(
        &evil_repo.join(".git").join("hooks").join("post-commit"),
        &sentinel,
    );

    // The worker rewrites the worktree's `.git` file to point at the attacker's repository.
    std::fs::write(
        created.path.join(".git"),
        format!("gitdir: {}\n", evil_repo.join(".git").display()),
    )
    .unwrap();

    std::fs::write(created.path.join("README.md"), "edited\n").unwrap();
    let commit = mgr
        .commit_all(&created.path, &created.git_dir, "agent changes")
        .await
        .unwrap()
        .expect("there was something to commit");

    let head = git_output_pinned(&created.path, &created.git_dir, &["rev-parse", "HEAD"]);
    assert_eq!(commit.sha, head);
    assert_ne!(
        head,
        rev_parse(&evil_repo, "HEAD"),
        "the commit must land in the real repository, not the redirected one"
    );
    assert!(
        !sentinel.exists(),
        "a hook from the redirected .git file must not have run"
    );
}
