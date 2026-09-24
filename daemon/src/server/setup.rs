//! What `serve` does before it accepts a connection (0007): check the data folder, take the lock,
//! clear an old socket, and bind a new one.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::StartError;

const LOCK_ATTEMPTS: usize = 5;

/// Creates the data folder if it is missing, checks that it is a folder, not a symlink, owned by
/// this process's effective user, and makes it 0700.
///
/// # Errors
///
/// [`StartError::DataDir`] if the folder can't be used safely, or [`StartError::Io`].
pub fn prepare_data_dir(dir: &Path) -> Result<(), StartError> {
    let io_error = |doing: &str, error| StartError::io(format!("{doing} {}", dir.display()), error);
    match fs::symlink_metadata(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|error| io_error("creating", error))?,
        Err(error) => return Err(io_error("reading", error)),
        Ok(_) => {}
    }
    let metadata = fs::symlink_metadata(dir).map_err(|error| io_error("reading", error))?;
    let refuse = |reason: String| StartError::DataDir {
        path: dir.to_owned(),
        reason,
    };
    if metadata.file_type().is_symlink() {
        return Err(refuse("it is a symlink".to_owned()));
    }
    if !metadata.is_dir() {
        return Err(refuse("it is not a folder".to_owned()));
    }
    let euid = rustix::process::geteuid().as_raw();
    if metadata.uid() != euid {
        return Err(refuse(format!(
            "it belongs to uid {}, not {euid}",
            metadata.uid()
        )));
    }
    if metadata.mode() & 0o7777 != 0o700 {
        fs::set_permissions(dir, Permissions::from_mode(0o700))
            .map_err(|error| io_error("making private", error))?;
    }
    Ok(())
}

/// The `flock` on `wispd.lock` that admits one `serve` per data folder.
#[derive(Debug)]
pub(crate) struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Locks the file at `path`, creating it if needed, and writes this process's pid into it.
    pub fn acquire(path: &Path, data_dir: &Path) -> Result<Self, StartError> {
        let io_error = |error| StartError::io(format!("locking {}", path.display()), error);
        // A stopping instance removes the file before it lets go of the lock, so the file locked
        // here may no longer be the one at `path`. Locking until it is keeps a second instance
        // from holding a lock on a file nobody else can find.
        for _ in 0..LOCK_ATTEMPTS {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(io_error)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(StartError::AlreadyRunning {
                        data_dir: data_dir.to_owned(),
                        pid: read_pid(&mut file),
                    });
                }
                Err(TryLockError::Error(error)) => return Err(io_error(error)),
            }
            let locked = file.metadata().map_err(io_error)?;
            let current = match fs::symlink_metadata(path) {
                Ok(current) => current,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error(error)),
            };
            if (locked.dev(), locked.ino()) != (current.dev(), current.ino()) {
                continue;
            }
            file.set_len(0).map_err(io_error)?;
            writeln!(file, "{}", std::process::id()).map_err(io_error)?;
            return Ok(Self {
                file,
                path: path.to_owned(),
            });
        }
        Err(io_error(io::Error::other(
            "the lock file kept changing while wispd locked it",
        )))
    }

    /// Removes the lock file, then lets go of the lock by closing it.
    pub fn release(self) {
        if let Err(error) = fs::remove_file(&self.path) {
            warn!(path = %self.path.display(), %error, "could not remove the lock file");
        }
        drop(self.file);
    }
}

fn read_pid(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

/// The socket this server bound, known by its inode, so wispd only ever removes its own.
#[derive(Debug)]
pub(crate) struct Socket {
    path: PathBuf,
    identity: (u64, u64),
}

impl Socket {
    /// Removes an old socket at `path`, but nothing that isn't a socket, then binds a new one
    /// there and makes it 0600. Only the holder of the instance lock may call it.
    pub fn bind(path: &Path) -> Result<(Self, UnixListener), StartError> {
        let io_error = |doing: &str, error| {
            StartError::io(format!("{doing} the socket {}", path.display()), error)
        };
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                fs::remove_file(path).map_err(|error| io_error("removing the old", error))?;
                info!(path = %path.display(), "removed an old socket");
            }
            Ok(_) => {
                return Err(StartError::NotASocket {
                    path: path.to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("reading", error)),
        }
        let (listener, identity) = bind_at(path).map_err(|error| io_error("binding", error))?;
        let socket = Self {
            path: path.to_owned(),
            identity,
        };
        Ok((socket, listener))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Binds again if the socket file is gone. macOS deletes old files in the per-user
    /// temporary folder that the fallback path uses, and a person may delete the socket too.
    pub fn rebind_if_gone(&mut self) -> io::Result<Option<UnixListener>> {
        match fs::symlink_metadata(&self.path) {
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (listener, identity) = bind_at(&self.path)?;
                self.identity = identity;
                Ok(Some(listener))
            }
            Err(error) => Err(error),
        }
    }

    /// Removes the socket file, if it is still the one this server bound.
    pub fn remove(&self) {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if (metadata.dev(), metadata.ino()) == self.identity => {
                if let Err(error) = fs::remove_file(&self.path) {
                    warn!(path = %self.path.display(), %error, "could not remove the socket");
                }
            }
            Ok(_) => {
                warn!(path = %self.path.display(), "left the socket path alone: another file is there now");
            }
            Err(_) => {}
        }
    }
}

// The directory is 0700 already, which keeps other users away from the socket between bind
// and chmod.
fn bind_at(path: &Path) -> io::Result<(UnixListener, (u64, u64))> {
    let listener = UnixListener::bind(path)?;
    let finish = || {
        fs::set_permissions(path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let metadata = fs::symlink_metadata(path)?;
        Ok((metadata.dev(), metadata.ino()))
    };
    match finish() {
        Ok(identity) => Ok((listener, identity)),
        Err(error) => {
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;

    use super::{InstanceLock, Socket, prepare_data_dir};
    use crate::server::StartError;

    #[test]
    fn the_data_folder_is_created_private_or_made_private() {
        let temp = tempfile::tempdir().unwrap();
        let fresh = temp.path().join("a/b");
        prepare_data_dir(&fresh).unwrap();
        assert_eq!(
            fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let open = temp.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, Permissions::from_mode(0o755)).unwrap();
        prepare_data_dir(&open).unwrap();
        assert_eq!(
            fs::metadata(&open).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn a_symlinked_or_non_folder_data_folder_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            prepare_data_dir(&link),
            Err(StartError::DataDir { .. })
        ));

        let file = temp.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(matches!(
            prepare_data_dir(&file),
            Err(StartError::DataDir { .. })
        ));
    }

    #[test]
    fn a_second_lock_is_refused_with_the_holders_pid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wispd.lock");
        let lock = InstanceLock::acquire(&path, temp.path()).unwrap();
        match InstanceLock::acquire(&path, temp.path()) {
            Err(StartError::AlreadyRunning { pid, .. }) => {
                assert_eq!(pid, Some(std::process::id()));
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
        lock.release();
        assert!(!path.exists());
        InstanceLock::acquire(&path, temp.path()).unwrap().release();
    }

    #[test]
    fn an_old_socket_is_replaced_and_anything_else_is_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wispd.sock");
        drop(UnixListener::bind(&path).unwrap());
        let (socket, _listener) = Socket::bind(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        socket.remove();
        assert!(!path.exists());

        fs::write(&path, "not a socket").unwrap();
        assert!(matches!(
            Socket::bind(&path),
            Err(StartError::NotASocket { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not a socket");
    }

    #[test]
    fn a_deleted_socket_is_bound_again_and_a_replaced_one_is_not_removed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wispd.sock");
        let (mut socket, _listener) = Socket::bind(&path).unwrap();
        assert!(socket.rebind_if_gone().unwrap().is_none());
        fs::remove_file(&path).unwrap();
        let _rebound = socket.rebind_if_gone().unwrap().expect("bound again");
        assert!(path.exists());

        fs::remove_file(&path).unwrap();
        fs::write(&path, "someone else's").unwrap();
        socket.remove();
        assert!(path.exists());
    }
}
