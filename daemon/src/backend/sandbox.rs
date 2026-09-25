//! The worker sandbox (0013): what a [`ToolPolicy::WorkspaceWrite`] run may write and read,
//! whichever backend runs it.
//!
//! A worker writes its cwd (its worktree), the project's shared context folder (0005), and its
//! own temp folder. Its commands can't write git metadata, can't read credential stores or
//! wispd's data folder, and get no network. Each backend turns a [`WorkerSandbox`] into its own
//! vendor's flags; wispd adds no OS sandbox of its own, because a vendor's sandbox can't start
//! inside one (0013).

use std::path::{Path, PathBuf};

use super::{RunRequest, StartError, ToolPolicy};

/// Credential stores in the home folder that no worker's commands may read (0013). Paths are
/// relative to the home folder. The list can't be complete: it names the stores wisp knows
/// about, and the vendors' own sandboxes add none by default.
pub const UNREADABLE_IN_HOME: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".docker",
    ".config/gh",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".claude",
    ".claude.json",
    ".codex",
    ".cursor",
    ".zsh_history",
    ".bash_history",
    "Library/Keychains",
];

/// A worker run's boundary beyond its cwd, which is always readable and writable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerSandbox {
    /// Folders the worker may read and write besides its cwd: the project's shared context
    /// folder (0005).
    pub writable: Vec<PathBuf>,
    /// Paths inside the writable folders that commands may read but not write: the worktree's
    /// `.git` file and the repository's git folder it points into. wispd commits for every
    /// backend (0013).
    pub read_only: Vec<PathBuf>,
    /// Paths commands may not read: [`UNREADABLE_IN_HOME`] and wispd's data folder. The cwd and
    /// [`WorkerSandbox::writable`] stay readable where they fall inside one of these.
    pub unreadable: Vec<PathBuf>,
}

impl WorkerSandbox {
    /// The v1 sandbox (0013) for a worker in `worktree`, a linked worktree whose `.git` file
    /// points into `git_dir`, the repository's shared git folder (`git rev-parse
    /// --git-common-dir`), and whose project's shared context folder is `context`. `home` is the
    /// user's home folder, and `data_dir` wispd's data folder, which holds both the worktree and
    /// the context folder.
    #[must_use]
    pub fn for_worktree(
        home: &Path,
        data_dir: &Path,
        worktree: &Path,
        git_dir: &Path,
        context: &Path,
    ) -> Self {
        let unreadable = UNREADABLE_IN_HOME
            .iter()
            .map(|path| home.join(path))
            .chain([data_dir.to_owned()])
            .collect();
        Self {
            writable: vec![context.to_owned()],
            read_only: vec![worktree.join(".git"), git_dir.to_owned()],
            unreadable,
        }
    }

    /// Every path, for checks that apply to all of them.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.writable
            .iter()
            .chain(&self.read_only)
            .chain(&self.unreadable)
            .map(PathBuf::as_path)
    }
}

/// The sandbox `request` runs in: `None` for a no-write run, which never writes, and the
/// request's [`WorkerSandbox`] for a worker.
///
/// # Errors
///
/// [`StartError::Invalid`] if a worker has no sandbox, which is how a caller that predates 0013
/// is refused, or if any of its paths is relative or not valid UTF-8, which the vendors'
/// settings can't carry.
pub fn worker_sandbox(request: &RunRequest) -> Result<Option<&WorkerSandbox>, StartError> {
    if request.policy == ToolPolicy::NoWrite {
        return Ok(None);
    }
    let Some(sandbox) = &request.sandbox else {
        return Err(StartError::Invalid(
            "a workspace-write run needs its worker sandbox (decision 0013)".into(),
        ));
    };
    if let Some(path) = sandbox
        .paths()
        .find(|path| !path.is_absolute() || path.to_str().is_none())
    {
        return Err(StartError::Invalid(format!(
            "the worker sandbox path {} is not an absolute UTF-8 path",
            path.display()
        )));
    }
    Ok(Some(sandbox))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{UNREADABLE_IN_HOME, WorkerSandbox};

    #[test]
    fn a_worktree_sandbox_writes_the_context_and_hides_secrets_and_the_data_folder() {
        let sandbox = WorkerSandbox::for_worktree(
            Path::new("/Users/u"),
            Path::new("/Users/u/Library/Application Support/wisp"),
            Path::new("/Users/u/Library/Application Support/wisp/worktrees/app-1a2b/run"),
            Path::new("/Users/u/src/app/.git"),
            Path::new("/Users/u/Library/Application Support/wisp/context/p"),
        );
        assert_eq!(
            sandbox.writable,
            [Path::new(
                "/Users/u/Library/Application Support/wisp/context/p"
            )]
        );
        assert_eq!(
            sandbox.read_only,
            [
                Path::new("/Users/u/Library/Application Support/wisp/worktrees/app-1a2b/run/.git"),
                Path::new("/Users/u/src/app/.git"),
            ]
        );
        assert!(sandbox.unreadable.contains(&"/Users/u/.ssh".into()));
        assert!(
            sandbox
                .unreadable
                .contains(&"/Users/u/Library/Application Support/wisp".into())
        );
        assert_eq!(sandbox.unreadable.len(), UNREADABLE_IN_HOME.len() + 1);
    }
}
