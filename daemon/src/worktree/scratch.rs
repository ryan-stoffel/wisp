//! Scratch repositories for normal threads with no repo (#110, decision 0017).
//!
//! Each such thread gets its own repository, made here, so the runner treats it like any other:
//! a worktree cut from its first commit, and wispd's commit when the CLI ends. The repository
//! is wispd's, not the user's, so it gets a local identity for those commits.

use std::path::Path;

use super::{WorktreeError, WorktreeManager};

/// The branch a scratch repository starts on.
const BRANCH: &str = "main";

/// The identity of commits in a scratch repository.
const NAME: &str = "wisp";
const EMAIL: &str = "wisp@localhost";

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
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "Start a wisp scratch folder",
            ],
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
            "wisp <wisp@localhost> Start a wisp scratch folder",
            "a second init keeps the first commit"
        );
        let created = manager
            .create(&scratch, wisp_protocol::RunId::generate(), None)
            .await
            .unwrap();
        assert!(created.path.join(".git").exists());
    }
}
