//! Scratch repositories for normal threads with no repo (#110, decision 0017).
//!
//! Each such thread gets its own repository, made here, so the runner treats it like any other:
//! a worktree cut from its first commit, and plxd's commit when the CLI ends. The repository
//! is plxd's, not the user's, so it gets a local identity for those commits.

use std::path::Path;

use super::{WorktreeError, WorktreeManager};

/// The branch a scratch repository starts on.
const BRANCH: &str = "main";

/// The identity of commits in a scratch repository.
const NAME: &str = "parallax";
const EMAIL: &str = "parallax@localhost";

impl WorktreeManager {
    /// Makes `path` a git repository with one empty commit on `main`, unless it already is one
    /// with a commit, as after a retried `thread/start`.
    ///
    /// # Errors
    ///
    /// [`WorktreeError::Io`] creating the folder, or [`WorktreeError::GitFailed`],
    /// [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`] from running git.
    pub async fn init_scratch(&self, path: &Path) -> Result<(), WorktreeError> {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|source| WorktreeError::Io {
                path: path.to_owned(),
                source,
            })?;
        if path.join(".git").is_dir()
            && self
                .run_git(path, &["rev-parse", "--verify", "--quiet", "HEAD"])
                .await?
                .success()
        {
            return Ok(());
        }
        let head = format!("refs/heads/{BRANCH}");
        self.run_git_ok(path, &["init", "--quiet"]).await?;
        self.run_git_ok(path, &["symbolic-ref", "HEAD", &head])
            .await?;
        self.run_git_ok(path, &["config", "user.name", NAME])
            .await?;
        self.run_git_ok(path, &["config", "user.email", EMAIL])
            .await?;
        self.run_git_ok(
            path,
            &[
                "-c",
                super::NO_HOOKS,
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "Start a parallax scratch folder",
            ],
        )
        .await?;
        Ok(())
    }

    /// Fetches `commit` and what it needs from the repository at `from` into the scratch
    /// repository at `path`, so a fork of a thread with no repo can cut its worktree from its
    /// parent's latest commit (0050).
    ///
    /// # Errors
    ///
    /// [`WorktreeError::GitFailed`], [`WorktreeError::Timeout`], or [`WorktreeError::Spawn`]
    /// from running git.
    pub async fn fetch_commit(
        &self,
        path: &Path,
        from: &Path,
        commit: &str,
    ) -> Result<(), WorktreeError> {
        let from = from.to_string_lossy();
        self.run_git_ok(
            path,
            &["fetch", "--quiet", "--no-tags", "--", &from, commit],
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::process::Launcher;
    use crate::paths::DataDir;
    use crate::worktree::WorktreeManager;

    #[tokio::test]
    async fn a_scratch_repository_has_one_commit_and_takes_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data")).unwrap();
        let launcher = Launcher::new(data.clone(), crate::agents::worker::agent_environment());
        let manager = WorktreeManager::new(launcher, data.root());
        let scratch = dir.path().join("data/scratch/run");
        manager.init_scratch(&scratch).await.unwrap();
        manager.init_scratch(&scratch).await.unwrap();

        let log = std::process::Command::new("git")
            .args(["log", "--format=%an <%ae> %s"])
            .current_dir(&scratch)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "parallax <parallax@localhost> Start a parallax scratch folder",
            "a second init keeps the first commit"
        );
        let created = manager
            .create(&scratch, parallax_protocol::RunId::generate(), None)
            .await
            .unwrap();
        assert!(created.path.join(".git").exists());

        // A fork's scratch repository takes its parent's latest commit, which no branch names.
        let commit = std::process::Command::new("git")
            .args(["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "work"])
            .current_dir(&scratch)
            .output()
            .unwrap();
        let commit = String::from_utf8(commit.stdout).unwrap().trim().to_owned();
        let fork = dir.path().join("data/scratch/fork");
        manager.init_scratch(&fork).await.unwrap();
        manager
            .fetch_commit(&fork, &scratch, &commit)
            .await
            .unwrap();
        let cut = manager
            .create(&fork, parallax_protocol::RunId::generate(), Some(&commit))
            .await
            .unwrap();
        assert_eq!(cut.base, commit);
    }
}
