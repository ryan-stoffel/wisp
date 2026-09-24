//! Where wispd keeps its files.
//!
//! Everything lives in one data folder, `~/Library/Application Support/wisp` by default (0006),
//! which the editor shares. wispd's own entries are:
//!
//! - `wispd.sock`: the socket, unless its path is too long (see [`DataDir::socket_path`]).
//! - `wispd.lock`: held with `flock` while a `wispd serve` runs. It contains that process's pid.
//! - `wispd.sqlite3`: the project store, with SQLite's `-wal` and `-shm` files next to it.
//! - `logs/wispd.log`: the log.
//!
//! `--data-dir` or [`DATA_DIR_ENV`] moves the whole folder. Every subcommand that reaches the
//! socket must resolve the folder and the socket path with [`DataDir`], so that `serve` and
//! `attach` always agree. Every process wispd starts is built with [`DataDir::command`], which
//! passes the folder on, so a `wispd` that an agent runs reaches the same socket.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// The environment variable that sets the data folder when `--data-dir` is not given.
pub const DATA_DIR_ENV: &str = "WISPD_DATA_DIR";

/// The longest socket path macOS accepts: `sun_path` holds 104 bytes, including the final NUL.
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

const DEFAULT_DATA_DIR: &str = "Library/Application Support/wisp";
const GETCONF: &str = "/usr/bin/getconf";

/// wispd's data folder, as an absolute path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// The data folder at `path`.
    ///
    /// A relative path is taken from the current directory. Symlinks are not resolved, and `.`
    /// components and trailing slashes are dropped, so every spelling of a folder names the same
    /// socket.
    ///
    /// # Errors
    ///
    /// If `path` is empty, or the current directory is needed and can't be read.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let absolute = std::path::absolute(path)?;
        Ok(Self {
            root: absolute.components().collect(),
        })
    }

    /// `~/Library/Application Support/wisp`.
    ///
    /// # Errors
    ///
    /// If the home folder is unknown.
    pub fn default_location() -> io::Result<Self> {
        let home = std::env::home_dir()
            .filter(|home| home.is_absolute())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the home folder is unknown"))?;
        Self::new(home.join(DEFAULT_DATA_DIR))
    }

    /// `path` if given, which is `--data-dir` or [`DATA_DIR_ENV`], else the default location.
    ///
    /// # Errors
    ///
    /// See [`DataDir::new`] and [`DataDir::default_location`].
    pub fn resolve(path: Option<&Path>) -> io::Result<Self> {
        match path {
            Some(path) => Self::new(path),
            None => Self::default_location(),
        }
    }

    /// The folder itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `wispd.lock`, which a running `serve` holds with `flock`.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.root.join("wispd.lock")
    }

    /// `wispd.sqlite3`, the project store.
    #[must_use]
    pub fn store_file(&self) -> PathBuf {
        self.root.join("wispd.sqlite3")
    }

    /// `logs/wispd.log`.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.root.join("logs").join("wispd.log")
    }

    /// A command for `program` with [`DATA_DIR_ENV`] set to this folder.
    ///
    /// Every process wispd starts is built with it, so a `wispd attach` or `wispd mcp` that an
    /// agent starts reaches this wispd's socket even when `serve` was given `--data-dir`.
    /// (Setting the variable on wispd's own environment instead is `unsafe` in Rust 2024.)
    #[must_use]
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env(DATA_DIR_ENV, &self.root);
        command
    }

    /// Where the socket goes.
    ///
    /// That is `wispd.sock` in the data folder, unless that path is longer than
    /// [`MAX_SOCKET_PATH_BYTES`], which happens when the home folder's path is longer than 59
    /// bytes. Then it is `wispd-<hash>.sock` in the folder that `getconf DARWIN_USER_TEMP_DIR`
    /// prints, which is per user and 0700. `<hash>` is the first 8 hex digits of the SHA-256 of
    /// the data folder's path, as [`DataDir::root`] spells it.
    ///
    /// # Errors
    ///
    /// When the fallback is needed and `getconf` fails, or the fallback path is too long too.
    pub fn socket_path(&self) -> io::Result<SocketPath> {
        let default = self.root.join("wispd.sock");
        if fits(&default) {
            return Ok(SocketPath {
                path: default,
                fallback: false,
            });
        }
        let path = self
            .darwin_user_temp_dir()?
            .join(format!("wispd-{}.sock", self.hash()));
        if !fits(&path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the socket path {} is longer than {MAX_SOCKET_PATH_BYTES} bytes",
                    path.display()
                ),
            ));
        }
        Ok(SocketPath {
            path,
            fallback: true,
        })
    }

    fn hash(&self) -> String {
        let digest = Sha256::digest(self.root.as_os_str().as_bytes());
        digest[..4].iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }

    // No safe wrapper for confstr(_CS_DARWIN_USER_TEMP_DIR) exists, and the workspace denies
    // unsafe code, so this asks getconf, as 0007 spells the rule. Only long home folders get
    // here.
    fn darwin_user_temp_dir(&self) -> io::Result<PathBuf> {
        let output = self.command(GETCONF).arg("DARWIN_USER_TEMP_DIR").output()?;
        let mut dir = output.stdout;
        while dir.last() == Some(&b'\n') {
            dir.pop();
        }
        if !output.status.success() || !dir.starts_with(b"/") {
            return Err(io::Error::other(format!(
                "`{GETCONF} DARWIN_USER_TEMP_DIR` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(PathBuf::from(OsString::from_vec(dir)))
    }
}

/// Where the socket goes, from [`DataDir::socket_path`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketPath {
    /// The socket's path.
    pub path: PathBuf,
    /// Whether this is the fallback in the per-user temporary folder. macOS deletes old files
    /// there, so the server checks that the socket still exists.
    pub fallback: bool,
}

fn fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DataDir, MAX_SOCKET_PATH_BYTES};

    #[test]
    fn spellings_of_one_folder_resolve_the_same() {
        let plain = DataDir::new("/Users/me/data").unwrap();
        for other in ["/Users/me/data/", "/Users/me/./data", "/Users/me//data"] {
            assert_eq!(DataDir::new(other).unwrap(), plain, "{other}");
        }
        assert_eq!(
            plain.hash(),
            DataDir::new("/Users/me/data/").unwrap().hash()
        );
        assert_ne!(
            plain.hash(),
            DataDir::new("/Users/me/data2").unwrap().hash()
        );
    }

    #[test]
    fn a_relative_folder_is_taken_from_the_current_directory() {
        let dir = DataDir::new("relative/data").unwrap();
        assert!(dir.root().is_absolute());
        assert!(dir.root().ends_with("relative/data"));
    }

    #[test]
    fn files_live_in_the_folder() {
        let dir = DataDir::new("/d").unwrap();
        assert_eq!(dir.lock_file(), Path::new("/d/wispd.lock"));
        assert_eq!(dir.store_file(), Path::new("/d/wispd.sqlite3"));
        assert_eq!(dir.log_file(), Path::new("/d/logs/wispd.log"));
    }

    #[test]
    fn commands_pass_the_data_folder_on() {
        let dir = DataDir::new("/tmp/wispd-data/./x/").unwrap();
        let output = dir.command("/usr/bin/env").output().unwrap();
        let env = String::from_utf8(output.stdout).unwrap();
        assert!(
            env.lines()
                .any(|line| line == "WISPD_DATA_DIR=/tmp/wispd-data/x"),
            "{env}"
        );
    }

    #[test]
    fn the_hash_is_the_first_8_hex_digits_of_the_paths_sha256() {
        // printf '%s' /Users/me/Library/Application\ Support/wisp | shasum -a 256
        let dir = DataDir::new("/Users/me/Library/Application Support/wisp").unwrap();
        assert_eq!(dir.hash(), "625c7f6d");
    }

    #[test]
    fn the_default_socket_is_used_up_to_103_bytes() {
        // "/Users/" and "/Library/Application Support/wisp/wispd.sock" leave 59 bytes for the
        // user name's folder, as 0007 says.
        let home = format!("/Users/{}", "u".repeat(52));
        assert_eq!(home.len(), 59);
        let dir = DataDir::new(format!("{home}/Library/Application Support/wisp")).unwrap();
        let socket = dir.socket_path().unwrap();
        assert!(!socket.fallback);
        assert_eq!(socket.path.as_os_str().len(), MAX_SOCKET_PATH_BYTES);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_longer_path_falls_back_to_the_user_temp_dir() {
        let home = format!("/Users/{}", "u".repeat(53));
        let dir = DataDir::new(format!("{home}/Library/Application Support/wisp")).unwrap();
        let socket = dir.socket_path().unwrap();
        assert!(socket.fallback);
        let temp = dir.darwin_user_temp_dir().unwrap();
        assert_eq!(socket.path, temp.join(format!("wispd-{}.sock", dir.hash())));
        assert!(socket.path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES);
    }
}
