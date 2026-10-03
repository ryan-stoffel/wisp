//! Where plxd keeps its files.
//!
//! Everything lives in one data folder (0009, 0023), which the editor shares:
//! `~/Library/Application Support/parallax` on macOS, `$XDG_DATA_HOME/parallax` or
//! `~/.local/share/parallax` on Linux, and `%LOCALAPPDATA%\parallax` on Windows. plxd's own entries are:
//!
//! - `plxd.sock`: the socket, unless its path is too long (see [`DataDir::socket_path`]). Windows
//!   listens on a named pipe instead, so there it isn't in the folder.
//! - `plxd.lock`: locked (`flock`, or `LockFileEx` on Windows) while a `plxd serve` runs. It
//!   contains that process's pid.
//! - `plxd.sqlite3`: the project store and the event log, with SQLite's `-wal` and `-shm` files
//!   next to it.
//! - `worktrees/`: agent runs' git worktrees (#154), and `context/`: shared context (#155).
//! - `tmp/`: files plxd writes for a run and deletes when it ends, such as a Claude worker's
//!   `CLAUDE_ENV_FILE` (RYA-126) and a Codex worker's `ZDOTDIR` (RYA-141). See
//!   [`DataDir::temp_dir`].
//! - `tools/`: CLIs plxd installs itself, which is only `gh` (`tools/gh/`, PLX-423). See
//!   [`DataDir::tools_dir`].
//! - `logs/plxd.log`: the log.
//!
//! One folder is outside it: `/tmp/parallax-<hash>/`, which holds each worker run's own temp folder
//! (RYA-130). See [`DataDir::run_temp_roots`].
//!
//! `--data-dir` or [`DATA_DIR_ENV`] moves the whole folder. Every subcommand that reaches the
//! socket must resolve the folder and the socket path with [`DataDir`], so that `serve` and
//! `attach` always agree. Every process plxd starts is built with [`DataDir::command`], which
//! passes the folder on, so a `plxd` that an agent runs reaches the same socket.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use parallax_protocol::ProjectId;
use sha2::{Digest, Sha256};

/// The environment variable that sets the data folder when `--data-dir` is not given.
pub const DATA_DIR_ENV: &str = "PLXD_DATA_DIR";

/// The longest socket path the OS accepts: `sun_path` holds 104 bytes on macOS, including the
/// final NUL.
#[cfg(target_os = "macos")]
pub const MAX_SOCKET_PATH_BYTES: usize = 103;
/// The longest socket path the OS accepts: `sun_path` holds 108 bytes on Linux, including the
/// final NUL.
#[cfg(target_os = "linux")]
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

/// The data folder under the home folder.
#[cfg(target_os = "macos")]
const DEFAULT_DATA_DIR: &str = "Library/Application Support/parallax";
/// The data folder under the home folder, when `XDG_DATA_HOME` doesn't name one.
#[cfg(target_os = "linux")]
const DEFAULT_DATA_DIR: &str = ".local/share/parallax";

#[cfg(target_os = "macos")]
const GETCONF: &str = "/usr/bin/getconf";

/// plxd's data folder, as an absolute path.
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

    /// The OS's data folder for Parallax (0023): `~/Library/Application Support/parallax` on macOS. On
    /// Linux, `$XDG_DATA_HOME/parallax`, or `~/.local/share/parallax` when `XDG_DATA_HOME` is unset or,
    /// as the XDG spec says, not absolute. On Windows, `%LOCALAPPDATA%\parallax`.
    ///
    /// # Errors
    ///
    /// If the home folder, or on Windows `LOCALAPPDATA`, is needed and unknown.
    #[cfg(windows)]
    pub fn default_location() -> io::Result<Self> {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "LOCALAPPDATA isn't set to an absolute path",
                )
            })?;
        Self::new(local.join("parallax"))
    }

    /// The OS's data folder for Parallax (0023): `~/Library/Application Support/parallax` on macOS. On
    /// Linux, `$XDG_DATA_HOME/parallax`, or `~/.local/share/parallax` when `XDG_DATA_HOME` is unset or,
    /// as the XDG spec says, not absolute. On Windows, `%LOCALAPPDATA%\parallax`.
    ///
    /// # Errors
    ///
    /// If the home folder is needed and unknown.
    #[cfg(unix)]
    pub fn default_location() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Self::new(data_home.join("parallax"));
        }
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

    /// `plxd.lock`, which a running `serve` holds locked.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.root.join("plxd.lock")
    }

    /// `plxd.sqlite3`, the project store.
    #[must_use]
    pub fn store_file(&self) -> PathBuf {
        self.root.join("plxd.sqlite3")
    }

    /// `logs/plxd.log`.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.root.join("logs").join("plxd.log")
    }

    /// `tmp/`: files plxd writes for a run and deletes when the run ends, such as a Claude
    /// worker's `CLAUDE_ENV_FILE` (RYA-126) and a Codex worker's `ZDOTDIR` (RYA-141), and `serve`
    /// sweeps at startup. No worker's commands can write them or read another run's, since the
    /// data folder is unreadable to every worker (0013). A Codex worker's own `ZDOTDIR` is
    /// readable to it.
    #[must_use]
    pub fn temp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Where each worker run gets its own temp folder (RYA-130), in order: `/tmp/parallax-<hash>`,
    /// then `parallax-<hash>` in `$TMPDIR`, for when `/tmp` can't be written, such as inside a
    /// worker's sandbox running plxd's own tests. `<hash>` is the socket fallback's, so each
    /// plxd has its own. They are outside the data folder because the path has to be short:
    /// Claude Code gives a worker's commands `$CLAUDE_CODE_TMPDIR/claude-<uid>` as `TMPDIR` only
    /// when that fits in 44 bytes. Windows, which runs no workers (0023), has only the second.
    #[must_use]
    pub fn run_temp_roots(&self) -> Vec<PathBuf> {
        let name = format!("parallax-{}", self.hash(4));
        let mut roots = vec![std::env::temp_dir().join(&name)];
        if cfg!(unix) {
            roots.insert(0, Path::new("/tmp").join(&name));
        }
        roots.dedup();
        roots
    }

    /// `tools/`: CLIs plxd installs itself, each in a folder of its own, such as `gh/` with
    /// `bin/gh` (PLX-423, 0050). Installs unpack into a temp folder inside it and are renamed into
    /// place.
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }

    /// The folder holding every project's shared context (0005, #155): `context/` in the data
    /// folder.
    #[must_use]
    pub fn context_root(&self) -> PathBuf {
        self.root.join("context")
    }

    /// One project's shared context folder: `context/<project-id>/` in the data folder. The
    /// runner (#156) makes it writable for the project's workers.
    #[must_use]
    pub fn context_dir(&self, project: ProjectId) -> PathBuf {
        self.context_root().join(project.to_string())
    }

    /// A command for `program` with [`DATA_DIR_ENV`] set to this folder.
    ///
    /// Every process plxd starts is built with it, so a `plxd attach` or `plxd mcp` that an
    /// agent starts reaches this plxd's socket even when `serve` was given `--data-dir`.
    /// (Setting the variable on plxd's own environment instead is `unsafe` in Rust 2024.)
    #[must_use]
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env(DATA_DIR_ENV, &self.root);
        command
    }

    /// Where the socket goes.
    ///
    /// That is `plxd.sock` in the data folder, unless that path is longer than
    /// [`MAX_SOCKET_PATH_BYTES`], which on macOS happens when the home folder's path is longer
    /// than 56 bytes. Then it is `plxd-<hash>.sock` in a per-user, 0700 folder (0023): the one
    /// `getconf DARWIN_USER_TEMP_DIR` prints on macOS, and `$XDG_RUNTIME_DIR` on Linux. `<hash>`
    /// is the first 8 hex digits of the SHA-256 of the data folder's path, as [`DataDir::root`]
    /// spells it.
    ///
    /// # Errors
    ///
    /// When the fallback is needed and its folder can't be found, or the fallback path is too
    /// long too.
    #[cfg(unix)]
    pub fn socket_path(&self) -> io::Result<SocketPath> {
        let default = self.root.join("plxd.sock");
        if fits(&default) {
            return Ok(SocketPath {
                path: default,
                fallback: false,
            });
        }
        let path = fallback_socket_dir()?.join(format!("plxd-{}.sock", self.hash(4)));
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

    /// The named pipe `serve` listens on (0023): `\\.\pipe\plxd-<hash>`, where `<hash>` is the
    /// first 16 hex digits of the SHA-256 of the data folder's path. Pipe names share one
    /// machine-wide namespace, hence twice the digits of the socket's fallback name. Only this
    /// user may connect; see [`crate::transport`].
    ///
    /// # Errors
    ///
    /// Never; the `Result` matches the Unix version.
    #[cfg(windows)]
    pub fn socket_path(&self) -> io::Result<SocketPath> {
        Ok(SocketPath {
            path: PathBuf::from(format!(r"\\.\pipe\plxd-{}", self.hash(8))),
            fallback: false,
        })
    }

    /// The first `bytes` bytes of the SHA-256 of the folder's path, in hex. On Windows, the path's
    /// bytes are its WTF-8 encoding, which is UTF-8 for any path that is valid Unicode.
    fn hash(&self, bytes: usize) -> String {
        let digest = Sha256::digest(self.root.as_os_str().as_encoded_bytes());
        digest[..bytes].iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }
}

// No safe wrapper for confstr(_CS_DARWIN_USER_TEMP_DIR) exists, and the workspace denies unsafe
// code, so this asks getconf, as 0007 spells the rule. Only long home folders get here.
#[cfg(target_os = "macos")]
fn fallback_socket_dir() -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let output = Command::new(GETCONF).arg("DARWIN_USER_TEMP_DIR").output()?;
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

// logind deletes the runtime folder at the last logout, while a `serve` that attach started keeps
// running, which is why it is only the fallback (0023). The server's check that rebinds a missing
// socket covers that.
#[cfg(target_os = "linux")]
fn fallback_socket_dir() -> io::Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the socket path in the data folder is longer than {MAX_SOCKET_PATH_BYTES} \
                     bytes, and XDG_RUNTIME_DIR isn't set to an absolute path for the fallback"
                ),
            )
        })
}

/// Where the socket goes, from [`DataDir::socket_path`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketPath {
    /// The socket's path, or on Windows the pipe's name.
    pub path: PathBuf,
    /// Whether this is the fallback in the per-user temporary or runtime folder. macOS deletes
    /// old files there, and logind deletes the runtime folder at the last logout, so the server
    /// checks that the socket still exists. Never on Windows.
    pub fallback: bool,
}

#[cfg(unix)]
fn fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES
}

/// `path` as Windows programs and the vendors' sandbox settings spell it, without the verbatim
/// `\\?\` prefix that [`Path::canonicalize`] puts on every path on Windows (RYA-109): `\\?\C:\x`
/// becomes `C:\x`, and `\\?\UNC\server\share\x` becomes `\\server\share\x`. A path without the
/// prefix is returned as it is, and so is every path on macOS and Linux.
///
/// A long path keeps its plain spelling: Rust's standard library puts the prefix back itself when
/// a path is too long for the Win32 limit.
///
/// `None` if only a verbatim path can name it, since removing the prefix would name another file:
/// a volume with no drive letter (`\\?\Volume{...}`), a device (`\\.\`), a drive's volume itself
/// (`\\?\C:`, not its root folder `\\?\C:\`), a UNC path with no share, or a component that ends
/// in a dot or a space, is `.` or `..`, holds a `/` or `:`, or is a reserved device name such as
/// `CON` or `NUL.txt`.
#[must_use]
pub fn without_verbatim_prefix(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_plain(path)
    }
    #[cfg(not(windows))]
    {
        Some(path.to_owned())
    }
}

#[cfg(windows)]
fn windows_plain(path: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Some(path.to_owned());
    };
    let mut plain = match prefix.kind() {
        Prefix::Disk(_) | Prefix::UNC(..) => return Some(path.to_owned()),
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return None,
        // `\\?\C:` is the volume, and `C:\` its root folder.
        Prefix::VerbatimDisk(_) if path.as_os_str().len() == prefix.as_os_str().len() => {
            return None;
        }
        Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", char::from(letter))),
        Prefix::VerbatimUNC(server, share) if server.is_empty() || share.is_empty() => {
            return None;
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut root = OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) if plain_name(name) => plain.push(name),
            _ => return None,
        }
    }
    Some(plain)
}

/// Device names Win32 reserves in every folder, alone or before an extension (`NUL.txt`).
#[cfg(windows)]
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Whether a path without the verbatim prefix still names the file called `name`: Win32 drops a
/// trailing dot or space, reads `/` as a separator and `D:` as a drive, and turns a reserved name
/// into a device. A name that isn't Unicode is checked with its unpaired surrogates replaced,
/// which keeps every character these rules look at.
#[cfg(windows)]
fn plain_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    let stem = name
        .split_once('.')
        .map_or(name.as_ref(), |(stem, _)| stem)
        .trim_end_matches(' ');
    !name.ends_with(['.', ' '])
        && !name.contains(['/', ':'])
        && !RESERVED_NAMES
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

#[cfg(test)]
mod tests {
    use super::DataDir;

    #[test]
    fn spellings_of_one_folder_resolve_the_same() {
        let plain = DataDir::new("/Users/me/data").unwrap();
        for other in ["/Users/me/data/", "/Users/me/./data", "/Users/me//data"] {
            assert_eq!(DataDir::new(other).unwrap(), plain, "{other}");
        }
        assert_eq!(
            plain.hash(4),
            DataDir::new("/Users/me/data/").unwrap().hash(4)
        );
        assert_ne!(
            plain.hash(4),
            DataDir::new("/Users/me/data2").unwrap().hash(4)
        );
    }

    #[test]
    fn a_relative_folder_is_taken_from_the_current_directory() {
        let dir = DataDir::new("relative/data").unwrap();
        assert!(dir.root().is_absolute());
        assert!(dir.root().ends_with("relative/data"));
    }

    #[cfg(unix)]
    #[test]
    fn files_live_in_the_folder() {
        use std::path::Path;

        let dir = DataDir::new("/d").unwrap();
        assert_eq!(dir.lock_file(), Path::new("/d/plxd.lock"));
        assert_eq!(dir.store_file(), Path::new("/d/plxd.sqlite3"));
        assert_eq!(dir.log_file(), Path::new("/d/logs/plxd.log"));
        assert_eq!(dir.context_root(), Path::new("/d/context"));
        let runs = format!("parallax-{}", dir.hash(4));
        assert_eq!(dir.run_temp_roots()[0], Path::new("/tmp").join(&runs));
        assert_eq!(
            dir.run_temp_roots().last(),
            Some(&std::env::temp_dir().join(runs))
        );
        let project = parallax_protocol::ProjectId::generate();
        assert_eq!(
            dir.context_dir(project),
            Path::new("/d/context").join(project.to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn commands_pass_the_data_folder_on() {
        let dir = DataDir::new("/tmp/plxd-data/./x/").unwrap();
        let output = dir.command("/usr/bin/env").output().unwrap();
        let env = String::from_utf8(output.stdout).unwrap();
        assert!(
            env.lines()
                .any(|line| line == "PLXD_DATA_DIR=/tmp/plxd-data/x"),
            "{env}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_hash_is_the_first_8_hex_digits_of_the_paths_sha256() {
        // printf '%s' /Users/me/Library/Application\ Support/parallax | shasum -a 256
        let dir = DataDir::new("/Users/me/Library/Application Support/parallax").unwrap();
        assert_eq!(dir.hash(4), "2786512e");
    }

    #[cfg(unix)]
    #[test]
    fn the_default_socket_is_used_up_to_the_limit() {
        use super::MAX_SOCKET_PATH_BYTES;

        let folder = "/Library/Application Support/parallax";
        let home = format!(
            "/Users/{}",
            "u".repeat(MAX_SOCKET_PATH_BYTES - "/Users//plxd.sock".len() - folder.len())
        );
        // On macOS, that leaves 56 bytes for the home folder, as 0007 says.
        #[cfg(target_os = "macos")]
        assert_eq!(home.len(), 56);
        let dir = DataDir::new(format!("{home}{folder}")).unwrap();
        let socket = dir.socket_path().unwrap();
        assert!(!socket.fallback);
        assert_eq!(socket.path.as_os_str().len(), MAX_SOCKET_PATH_BYTES);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_longer_path_falls_back_to_the_user_temp_dir() {
        use super::MAX_SOCKET_PATH_BYTES;

        let home = format!("/Users/{}", "u".repeat(53));
        let dir = DataDir::new(format!("{home}/Library/Application Support/parallax")).unwrap();
        let socket = dir.socket_path().unwrap();
        assert!(socket.fallback);
        let temp = super::fallback_socket_dir().unwrap();
        assert_eq!(socket.path, temp.join(format!("plxd-{}.sock", dir.hash(4))));
        assert!(socket.path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES);
    }

    #[cfg(windows)]
    #[test]
    fn the_pipe_is_named_after_16_hex_digits_of_the_paths_sha256() {
        // printf %s 'C:\parallax' | shasum -a 256
        let dir = DataDir::new(r"C:\parallax").unwrap();
        assert_eq!(dir.hash(8), "7dd38c3ef373388d");
        let socket = dir.socket_path().unwrap();
        assert_eq!(socket.path.as_os_str(), r"\\.\pipe\plxd-7dd38c3ef373388d");
        assert!(!socket.fallback);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_lose_their_prefix() {
        use std::path::Path;

        use super::without_verbatim_prefix;

        let plain = |path: &str| without_verbatim_prefix(Path::new(path));
        for (verbatim, expected) in [
            (r"\\?\C:\Users\runneradmin", r"C:\Users\runneradmin"),
            (r"\\?\d:\", r"D:\"),
            (r"\\?\C:\a b\.git\x.y", r"C:\a b\.git\x.y"),
            (r"\\?\UNC\server\share\repo", r"\\server\share\repo"),
            (r"\\?\UNC\server\share", r"\\server\share\"),
            // Already plain
            (r"C:\Users\me", r"C:\Users\me"),
            (r"\\server\share\repo", r"\\server\share\repo"),
        ] {
            assert_eq!(
                plain(verbatim).as_deref(),
                Some(Path::new(expected)),
                "{verbatim}"
            );
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(&long).unwrap().as_os_str(), &long[4..]);
    }

    /// Paths a plain spelling would send to another file, or to a device.
    #[cfg(windows)]
    #[test]
    fn a_path_only_a_verbatim_prefix_can_name_is_refused() {
        use std::path::Path;

        use super::without_verbatim_prefix;

        for verbatim in [
            r"\\?\Volume{0b8e7c8d-0000-0000-0000-100000000000}\repo",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\repo",
            r"\\.\COM1",
            r"\\?\C:\repo.",
            r"\\?\C:\repo \x",
            r"\\?\C:\a\..\b",
            r"\\?\C:\a\.\b",
            r"\\?\C:\a/b",
            r"\\?\C:\CON",
            r"\\?\C:\x\nul.txt",
            r"\\?\C:\Lpt1\x",
            r"\\?\C:\com¹",
            r"\\?\UNC\server\share\aux",
            // `PathBuf::push` reads `a:` as a drive and drops what came before it
            r"\\?\C:\x\a:b",
            r"\\?\C:\x\D:",
            // Win32 reads `CON:` as the console
            r"\\?\C:\x\CON:s",
            // The volume, not its root folder
            r"\\?\C:",
            r"\\?\UNC\server",
            r"\\?\UNC\server\",
        ] {
            assert_eq!(
                without_verbatim_prefix(Path::new(verbatim)),
                None,
                "{verbatim}"
            );
        }
        for fine in [
            r"\\?\C:\console",
            r"\\?\C:\CONFIG",
            r"\\?\C:\com10",
            r"\\?\C:\.con",
        ] {
            assert!(without_verbatim_prefix(Path::new(fine)).is_some(), "{fine}");
        }
    }

    /// A name that isn't Unicode, here with an unpaired surrogate, gets the same checks.
    #[cfg(windows)]
    #[test]
    fn a_name_that_isnt_unicode_is_checked_too() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use std::path::PathBuf;

        use super::without_verbatim_prefix;

        let with_surrogate = |prefix: &str, suffix: &str| {
            let mut wide: Vec<u16> = prefix.encode_utf16().collect();
            wide.push(0xD800);
            wide.extend(suffix.encode_utf16());
            PathBuf::from(OsString::from_wide(&wide))
        };
        for suffix in [".", " ", ":s", "/x"] {
            let path = with_surrogate(r"\\?\C:\x\a", suffix);
            assert_eq!(without_verbatim_prefix(&path), None, "{}", path.display());
        }
        assert_eq!(
            without_verbatim_prefix(&with_surrogate(r"\\?\C:\x\a", "b")),
            Some(with_surrogate(r"C:\x\a", "b"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_default_folder_is_parallax_in_local_app_data() {
        let local = std::env::var_os("LOCALAPPDATA").unwrap();
        assert_eq!(
            DataDir::default_location().unwrap().root(),
            std::path::Path::new(&local).join("parallax")
        );
    }
}
