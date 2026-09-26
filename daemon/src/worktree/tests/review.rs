//! Reviewing and accepting a run's commit (#157), against real temporary repositories.

use std::path::{Path, PathBuf};

use wisp_protocol::RunId;

use super::{git, git_output, init_repo, manager, rev_parse, write_sentinel_script};
use crate::worktree::{
    AcceptError, ChangeStatus, CreatedWorktree, MAX_BLOB_BYTES, MergeHow, WorktreeManager,
};

struct Fixture {
    _repo_dir: tempfile::TempDir,
    _data_dir: tempfile::TempDir,
    repo: PathBuf,
    mgr: WorktreeManager,
    created: CreatedWorktree,
}

async fn fixture() -> Fixture {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = init_repo(repo_dir.path()).canonicalize().unwrap();
    std::fs::write(repo.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("old.txt"), "going away\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "more files"]);
    let data_dir = tempfile::tempdir().unwrap();
    let mgr = manager(data_dir.path());
    let created = mgr.create(&repo, RunId::generate(), None).await.unwrap();
    Fixture {
        _repo_dir: repo_dir,
        _data_dir: data_dir,
        repo,
        mgr,
        created,
    }
}

impl Fixture {
    fn worktree(&self) -> &Path {
        &self.created.path
    }

    async fn commit(&self) -> String {
        self.mgr
            .commit_all(
                &self.created.path,
                &self.created.git_dir,
                &self.repo,
                "agent work",
            )
            .await
            .unwrap()
            .expect("a commit")
            .sha
    }

    async fn accept(&self, commit: &str) -> Result<crate::worktree::Accepted, AcceptError> {
        self.mgr
            .accept(&self.repo, commit, "Merge wisp run: test\n")
            .await
    }
}

#[tokio::test]
async fn diff_commits_lists_each_file_with_its_stats_and_diff() {
    let f = fixture().await;
    let base = f.created.base.clone();
    assert_eq!(
        f.mgr
            .diff_commits(f.worktree(), &f.created.git_dir, &base, &base)
            .await
            .unwrap()
            .files,
        [],
        "no commit yet: nothing to review"
    );

    std::fs::write(f.worktree().join("README.md"), "hello\nworld\n").unwrap();
    std::fs::write(f.worktree().join("new file.md"), "# New\n").unwrap();
    std::fs::remove_file(f.worktree().join("old.txt")).unwrap();
    std::fs::rename(
        f.worktree().join("notes.txt"),
        f.worktree().join("moved.txt"),
    )
    .unwrap();
    std::fs::write(f.worktree().join("logo.bin"), [0_u8, 159, 146, 150, 0]).unwrap();
    let head = f.commit().await;

    let diff = f
        .mgr
        .diff_commits(f.worktree(), &f.created.git_dir, &base, &head)
        .await
        .unwrap();
    let summary: Vec<_> = diff
        .files
        .iter()
        .map(|file| {
            (
                file.status.clone(),
                file.path.as_str(),
                file.old_path.as_deref(),
                file.insertions,
                file.deletions,
                file.binary,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (ChangeStatus::Modified, "README.md", None, 1, 0, false),
            (ChangeStatus::Added, "logo.bin", None, 0, 0, true),
            (
                ChangeStatus::Renamed,
                "moved.txt",
                Some("notes.txt"),
                0,
                0,
                false
            ),
            (ChangeStatus::Added, "new file.md", None, 1, 0, false),
            (ChangeStatus::Deleted, "old.txt", None, 0, 1, false),
        ]
    );
    assert_eq!(diff.stat.files, 5);
    assert_eq!(diff.stat.insertions, 2);
    assert_eq!(diff.stat.deletions, 1);
    assert!(!diff.truncated);
    let readme = diff.files[0].diff.as_deref().unwrap();
    assert!(
        readme.starts_with("diff --git a/README.md b/README.md\n"),
        "{readme}"
    );
    assert!(readme.contains("\n+world\n"), "{readme}");
    assert_eq!(diff.files[1].diff, None, "a binary file has no text diff");
    assert!(
        diff.files[4]
            .diff
            .as_deref()
            .unwrap()
            .contains("-going away")
    );
}

#[tokio::test]
async fn a_huge_file_diff_is_cut_short_but_keeps_its_stats() {
    let f = fixture().await;
    let big = "a line of text\n".repeat(40_000);
    std::fs::write(f.worktree().join("big.txt"), &big).unwrap();
    std::fs::write(f.worktree().join("small.txt"), "small\n").unwrap();
    let head = f.commit().await;
    let diff = f
        .mgr
        .diff_commits(f.worktree(), &f.created.git_dir, &f.created.base, &head)
        .await
        .unwrap();
    let big_file = &diff.files[0];
    assert_eq!(big_file.path, "big.txt");
    assert_eq!(big_file.insertions, 40_000);
    assert!(big_file.diff_truncated);
    assert!(big_file.diff.as_ref().unwrap().len() <= 256 * 1024);
    assert_eq!(
        diff.files[1].diff.as_deref().map(|d| d.contains("+small")),
        Some(true)
    );
}

#[tokio::test]
async fn read_blob_returns_either_side_byte_for_byte() {
    let f = fixture().await;
    let base = f.created.base.clone();
    let exact: &[(&str, &[u8])] = &[
        ("no-newline.txt", b"last line has no newline"),
        ("crlf.txt", b"one\r\ntwo\r\n"),
        ("binary.bin", &[0, 10, 255, 10, 10, 0, 13]),
        ("empty.txt", b""),
        ("trailing.txt", b"two newlines\n\n"),
    ];
    for (name, bytes) in exact {
        std::fs::write(f.worktree().join(name), bytes).unwrap();
    }
    std::fs::write(f.worktree().join("README.md"), "changed\n").unwrap();
    std::fs::create_dir_all(f.worktree().join("dir")).unwrap();
    std::fs::write(f.worktree().join("dir/inner.md"), "inner\n").unwrap();
    std::fs::write(f.worktree().join("*.md"), "literally star\n").unwrap();
    let head = f.commit().await;
    let (path, git_dir) = (f.worktree(), &f.created.git_dir);

    for (name, bytes) in exact {
        let blob = f
            .mgr
            .read_blob(path, git_dir, &head, name, MAX_BLOB_BYTES)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blob.content.as_deref(), Some(*bytes), "{name}");
        assert_eq!(blob.size, bytes.len() as u64, "{name}");
    }
    let base_readme = f
        .mgr
        .read_blob(path, git_dir, &base, "README.md", MAX_BLOB_BYTES)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(base_readme.content.as_deref(), Some(&b"hello\n"[..]));
    assert_eq!(
        f.mgr
            .read_blob(path, git_dir, &base, "no-newline.txt", MAX_BLOB_BYTES)
            .await
            .unwrap(),
        None,
        "added files have no base side"
    );
    assert_eq!(
        f.mgr
            .read_blob(path, git_dir, &head, "dir", MAX_BLOB_BYTES)
            .await
            .unwrap(),
        None,
        "a folder is not a file"
    );
    let star = f
        .mgr
        .read_blob(path, git_dir, &head, "*.md", MAX_BLOB_BYTES)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        star.content.as_deref(),
        Some(&b"literally star\n"[..]),
        "paths are literal, never patterns"
    );
    let capped = f
        .mgr
        .read_blob(path, git_dir, &head, "crlf.txt", 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((capped.size, capped.content), (10, None));
}

#[tokio::test]
async fn a_committed_symlink_reads_as_its_target_and_is_never_followed() {
    let f = fixture().await;
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "do not read me\n").unwrap();
    std::os::unix::fs::symlink(&secret, f.worktree().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), f.worktree().join("dirlink")).unwrap();
    let head = f.commit().await;

    let link = f
        .mgr
        .read_blob(
            f.worktree(),
            &f.created.git_dir,
            &head,
            "link",
            MAX_BLOB_BYTES,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        link.content.as_deref(),
        Some(secret.to_string_lossy().as_bytes())
    );
    assert_eq!(
        f.mgr
            .read_blob(
                f.worktree(),
                &f.created.git_dir,
                &head,
                "dirlink/secret.txt",
                MAX_BLOB_BYTES
            )
            .await
            .unwrap(),
        None,
        "a path through a symlinked folder does not exist in git's tree"
    );
}

#[tokio::test]
async fn accept_fast_forwards_the_current_branch_and_keeps_unrelated_uncommitted_work() {
    let f = fixture().await;
    std::fs::write(f.worktree().join("README.md"), "hello from the agent\n").unwrap();
    let commit = f.commit().await;
    std::fs::write(f.repo.join("notes.txt"), "one\ntwo\nthree\nmine\n").unwrap();
    std::fs::write(f.repo.join("scratch.txt"), "untracked\n").unwrap();
    git(&f.repo, &["add", "notes.txt"]);

    let accepted = f.accept(&commit).await.unwrap();
    assert_eq!(accepted.how, MergeHow::FastForward);
    assert_eq!(accepted.into, "main");
    assert_eq!(accepted.commit, commit);
    assert_eq!(rev_parse(&f.repo, "HEAD"), commit);
    assert_eq!(
        std::fs::read_to_string(f.repo.join("README.md")).unwrap(),
        "hello from the agent\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.repo.join("notes.txt")).unwrap(),
        "one\ntwo\nthree\nmine\n",
        "a staged change to another file stays"
    );
    assert!(f.repo.join("scratch.txt").exists());
    assert_eq!(
        git_output(&f.repo, &["diff", "--cached", "--name-only"]),
        "notes.txt"
    );

    let again = f.accept(&commit).await.unwrap();
    assert_eq!(again.how, MergeHow::UpToDate, "a retry changes nothing");
    assert_eq!(again.commit, commit);
}

#[tokio::test]
async fn accept_makes_a_merge_commit_when_the_branch_moved_on() {
    let f = fixture().await;
    std::fs::write(f.worktree().join("README.md"), "hello from the agent\n").unwrap();
    let commit = f.commit().await;
    std::fs::write(f.repo.join("notes.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&f.repo, &["commit", "-q", "-am", "user work"]);
    let user_head = rev_parse(&f.repo, "HEAD");

    let accepted = f.accept(&commit).await.unwrap();
    assert_eq!(accepted.how, MergeHow::Merge);
    assert_eq!(rev_parse(&f.repo, "HEAD"), accepted.commit);
    assert_eq!(
        git_output(&f.repo, &["log", "-1", "--format=%P"]),
        format!("{user_head} {commit}")
    );
    assert_eq!(
        git_output(&f.repo, &["log", "-1", "--format=%s%n%an <%ae>"]),
        "Merge wisp run: test\nTest User <test@example.com>"
    );
    assert_eq!(
        std::fs::read_to_string(f.repo.join("README.md")).unwrap(),
        "hello from the agent\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.repo.join("notes.txt")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(git_output(&f.repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn accept_refuses_uncommitted_changes_to_a_file_the_agent_changed() {
    let f = fixture().await;
    std::fs::write(f.worktree().join("README.md"), "hello from the agent\n").unwrap();
    std::fs::write(f.worktree().join("added.txt"), "agent\n").unwrap();
    let commit = f.commit().await;
    let head = rev_parse(&f.repo, "HEAD");

    std::fs::write(f.repo.join("README.md"), "my edit\n").unwrap();
    let error = f.accept(&commit).await.unwrap_err();
    let AcceptError::Refused(message) = &error else {
        panic!("{error:?}");
    };
    assert!(message.contains("README.md"), "{message}");
    assert_eq!(rev_parse(&f.repo, "HEAD"), head);
    assert_eq!(
        std::fs::read_to_string(f.repo.join("README.md")).unwrap(),
        "my edit\n"
    );

    git(&f.repo, &["checkout", "--", "README.md"]);
    std::fs::write(f.repo.join("added.txt"), "mine, untracked\n").unwrap();
    let error = f.accept(&commit).await.unwrap_err();
    assert!(
        matches!(&error, AcceptError::Refused(message) if message.contains("added.txt")),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(f.repo.join("added.txt")).unwrap(),
        "mine, untracked\n"
    );
    assert_eq!(rev_parse(&f.repo, "HEAD"), head);
}

#[tokio::test]
async fn accept_refuses_conflicting_changes_and_leaves_the_checkout_alone() {
    let f = fixture().await;
    std::fs::write(f.worktree().join("notes.txt"), "one\nTWO (agent)\nthree\n").unwrap();
    let commit = f.commit().await;
    std::fs::write(f.repo.join("notes.txt"), "one\ntwo (user)\nthree\n").unwrap();
    git(&f.repo, &["commit", "-q", "-am", "user work"]);
    let head = rev_parse(&f.repo, "HEAD");

    let error = f.accept(&commit).await.unwrap_err();
    let AcceptError::Conflict { into, paths } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(into, "main");
    assert_eq!(paths, &["notes.txt"]);
    assert_eq!(rev_parse(&f.repo, "HEAD"), head);
    assert_eq!(git_output(&f.repo, &["status", "--porcelain"]), "");
    assert!(!f.repo.join(".git/MERGE_HEAD").exists());
}

#[tokio::test]
async fn accept_refuses_a_detached_head_or_an_operation_in_progress() {
    let f = fixture().await;
    std::fs::write(f.worktree().join("README.md"), "agent\n").unwrap();
    let commit = f.commit().await;
    let head = rev_parse(&f.repo, "HEAD");

    git(&f.repo, &["checkout", "-q", "--detach"]);
    let error = f.accept(&commit).await.unwrap_err();
    assert!(
        matches!(&error, AcceptError::Refused(message) if message.contains("detached")),
        "{error:?}"
    );
    git(&f.repo, &["checkout", "-q", "main"]);

    std::fs::write(f.repo.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
    let error = f.accept(&commit).await.unwrap_err();
    assert!(
        matches!(&error, AcceptError::Refused(message) if message.contains("a merge")),
        "{error:?}"
    );
    assert_eq!(rev_parse(&f.repo, "HEAD"), head);
}

#[tokio::test]
async fn accept_runs_no_hooks_even_ones_the_agent_committed() {
    let f = fixture().await;
    // A second run, created before any hook exists, for the merge-commit path below. (`git
    // worktree add` runs the repository's own post-checkout hook, which is #154's behavior.)
    let second = f
        .mgr
        .create(&f.repo, RunId::generate(), None)
        .await
        .unwrap();
    let sentinels = tempfile::tempdir().unwrap();
    let agent_hook = sentinels.path().join("agent-hook-ran");
    let user_hook = sentinels.path().join("user-hook-ran");
    // The repository points hooks at a tracked folder, as husky does, and the agent adds a
    // post-merge hook there. The user also has one of their own in .git/hooks.
    git(&f.repo, &["config", "core.hooksPath", ".husky"]);
    write_sentinel_script(&f.worktree().join(".husky/post-merge"), &agent_hook);
    write_sentinel_script(&f.worktree().join(".husky/post-checkout"), &agent_hook);
    write_sentinel_script(
        &f.worktree().join(".husky/reference-transaction"),
        &agent_hook,
    );
    std::fs::write(f.worktree().join("README.md"), "agent\n").unwrap();
    let commit = f.commit().await;
    write_sentinel_script(&f.repo.join(".git/hooks/post-merge"), &user_hook);

    let accepted = f.accept(&commit).await.unwrap();
    assert_eq!(accepted.how, MergeHow::FastForward);
    assert!(f.repo.join(".husky/post-merge").exists(), "merged");
    assert!(!agent_hook.exists(), "the agent's hook must not run");
    assert!(!user_hook.exists(), "no hook runs during a headless accept");

    // A merge commit takes the other path, through merge-tree and commit-tree.
    std::fs::write(f.repo.join("notes.txt"), "user\n").unwrap();
    git(
        &f.repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "-am",
            "user",
        ],
    );
    std::fs::write(second.path.join("second.txt"), "agent\n").unwrap();
    let second_commit = f
        .mgr
        .commit_all(&second.path, &second.git_dir, &f.repo, "agent")
        .await
        .unwrap()
        .unwrap()
        .sha;
    std::fs::write(f.repo.join("notes.txt"), "user again\n").unwrap();
    git(
        &f.repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "-am",
            "user again",
        ],
    );
    let merged = f
        .mgr
        .accept(&f.repo, &second_commit, "Merge wisp run: second\n")
        .await
        .unwrap();
    assert_eq!(merged.how, MergeHow::Merge);
    assert!(!agent_hook.exists());
    assert!(!user_hook.exists());
}
