//! A project's repository on this host: whether a folder is the top of a git working tree, and
//! which branch it has checked out. It reads the files git keeps and never runs git.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The most of `HEAD` or a `.git` file that is read. Both are one short line.
const MAX_POINTER_BYTES: u64 = 4096;

/// Why a folder can't back a project. The message is shown to the user as is.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NotARepository {
    #[error("{0} doesn't exist on this host.")]
    Missing(String),
    #[error("{0} is a file, not a folder.")]
    NotAFolder(String),
    #[error("{0} is not the top folder of a git repository: it has no .git.")]
    NoGit(String),
    #[error("{0}/.git is not a git folder, or a .git file that points to one.")]
    BrokenGit(String),
    #[error("{0} can't be read: {1}")]
    Unreadable(String, String),
}

/// Checks that `path` is the top folder of a git working tree: it has a `.git` folder with a
/// `HEAD`, or a `.git` file that points to one, as a linked worktree or a submodule has. Returns
/// that git folder.
pub(crate) fn check(path: &Path) -> Result<PathBuf, NotARepository> {
    let shown = || path.display().to_string();
    let unreadable = |error: io::Error| NotARepository::Unreadable(shown(), error.to_string());
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(NotARepository::NotAFolder(shown())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(NotARepository::Missing(shown()));
        }
        Err(error) => return Err(unreadable(error)),
    }
    let dot_git = path.join(".git");
    let git_dir = match fs::metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => dot_git,
        Ok(metadata) if metadata.is_file() => {
            let pointer = read_short(&dot_git).map_err(unreadable)?;
            let target = pointer
                .strip_prefix("gitdir:")
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .ok_or_else(|| NotARepository::BrokenGit(shown()))?;
            path.join(target)
        }
        Ok(_) => return Err(NotARepository::BrokenGit(shown())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(NotARepository::NoGit(shown()));
        }
        Err(error) => return Err(unreadable(error)),
    };
    if fs::metadata(git_dir.join("HEAD")).is_ok_and(|metadata| metadata.is_file()) {
        Ok(git_dir)
    } else {
        Err(NotARepository::BrokenGit(shown()))
    }
}

/// The branch checked out at `path`, or the first 7 digits of the commit when `HEAD` is
/// detached. `None` when `path` is not a repository or its `HEAD` can't be read.
pub(crate) fn branch(path: &Path) -> Option<String> {
    let head = read_short(&check(path).ok()?.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!name.is_empty()).then(|| name.to_owned());
    }
    let is_commit =
        matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit());
    is_commit.then(|| head[..7].to_owned())
}

fn read_short(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{NotARepository, branch, check};

    fn repo(root: &Path, head: &str) -> std::path::PathBuf {
        let path = root.join("repo");
        fs::create_dir_all(path.join(".git")).unwrap();
        fs::write(path.join(".git/HEAD"), head).unwrap();
        path
    }

    #[test]
    fn a_folder_with_a_git_folder_is_a_repository_on_its_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo(dir.path(), "ref: refs/heads/main\n");
        assert_eq!(check(&path), Ok(path.join(".git")));
        assert_eq!(branch(&path).as_deref(), Some("main"));

        fs::write(
            path.join(".git/HEAD"),
            "ref: refs/heads/feature/104-projects\n",
        )
        .unwrap();
        assert_eq!(branch(&path).as_deref(), Some("feature/104-projects"));
    }

    #[test]
    fn a_detached_head_is_its_short_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo(dir.path(), "4ecd1f00a1b2c3d4e5f60718293a4b5c6d7e8f90\n");
        assert_eq!(branch(&path).as_deref(), Some("4ecd1f0"));

        fs::write(path.join(".git/HEAD"), "not a commit\n").unwrap();
        assert_eq!(branch(&path), None);
    }

    #[test]
    fn a_git_file_points_to_the_git_folder_of_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = repo(dir.path(), "ref: refs/heads/main\n");
        let linked_git = main.join(".git/worktrees/linked");
        fs::create_dir_all(&linked_git).unwrap();
        fs::write(linked_git.join("HEAD"), "ref: refs/heads/agent-1\n").unwrap();

        let absolute = dir.path().join("absolute");
        fs::create_dir(&absolute).unwrap();
        fs::write(
            absolute.join(".git"),
            format!("gitdir: {}\n", linked_git.display()),
        )
        .unwrap();
        assert_eq!(check(&absolute), Ok(linked_git.clone()));
        assert_eq!(branch(&absolute).as_deref(), Some("agent-1"));

        let relative = dir.path().join("relative");
        fs::create_dir(&relative).unwrap();
        fs::write(
            relative.join(".git"),
            "gitdir: ../repo/.git/worktrees/linked\n",
        )
        .unwrap();
        assert_eq!(branch(&relative).as_deref(), Some("agent-1"));
    }

    #[test]
    fn anything_else_is_not_a_repository_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let shown = |path: &Path| path.display().to_string();

        let missing = root.join("missing");
        assert_eq!(
            check(&missing),
            Err(NotARepository::Missing(shown(&missing)))
        );

        let file = root.join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(check(&file), Err(NotARepository::NotAFolder(shown(&file))));

        let plain = root.join("plain");
        fs::create_dir(&plain).unwrap();
        assert_eq!(check(&plain), Err(NotARepository::NoGit(shown(&plain))));
        assert_eq!(branch(&plain), None);

        let no_head = root.join("no-head");
        fs::create_dir_all(no_head.join(".git")).unwrap();
        assert_eq!(
            check(&no_head),
            Err(NotARepository::BrokenGit(shown(&no_head)))
        );

        let bad_pointer = root.join("bad-pointer");
        fs::create_dir(&bad_pointer).unwrap();
        fs::write(bad_pointer.join(".git"), "not a pointer").unwrap();
        assert_eq!(
            check(&bad_pointer),
            Err(NotARepository::BrokenGit(shown(&bad_pointer)))
        );

        let dangling = root.join("dangling");
        fs::create_dir(&dangling).unwrap();
        fs::write(dangling.join(".git"), "gitdir: /nowhere/at/all").unwrap();
        assert_eq!(
            check(&dangling),
            Err(NotARepository::BrokenGit(shown(&dangling)))
        );

        assert_eq!(
            NotARepository::NoGit("/src/wisp".to_owned()).to_string(),
            "/src/wisp is not the top folder of a git repository: it has no .git."
        );
    }
}
