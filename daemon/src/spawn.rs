//! Starting a process in its own session with no descriptors but its stdio: `serve`, which
//! `attach` starts detached (0010), and the agent CLIs that `backend::process` supervises.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::iter;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use nix::libc;
use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};
use nix::sys::signal::SigSet;
use rustix::process::Pid;
use zeroize::Zeroize;

/// `POSIX_SPAWN_SETSID` from `<sys/spawn.h>`, which the libc crate doesn't define for Apple
/// platforms.
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// A detached process's stdin, stdout, and stderr, which are its only descriptors.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stdio<'a> {
    pub stdin: BorrowedFd<'a>,
    pub stdout: BorrowedFd<'a>,
    pub stderr: BorrowedFd<'a>,
}

/// Starts `command`'s program, with its arguments and environment, detached from this process:
///
/// - In a new session, so no terminal or SSH session this process belongs to can signal it.
/// - With `stdio` as descriptors 0 to 2 and no others. `POSIX_SPAWN_CLOEXEC_DEFAULT` closes
///   everything else, including descriptors this process inherited without close-on-exec and
///   ones another thread has just opened (#86).
/// - With every signal at its default action and none blocked.
///
/// The child keeps this process's working directory, so `command` must not set one. The caller
/// reaps the child with `waitpid`, or exits first and leaves that to launchd.
///
/// # Errors
///
/// If an argument or environment variable contains a NUL byte, or `posix_spawn` fails.
pub(crate) fn spawn_detached(command: &Command, stdio: Stdio<'_>) -> io::Result<Pid> {
    debug_assert!(
        command.get_current_dir().is_none(),
        "a detached process keeps the working directory"
    );
    let argv: Vec<&OsStr> = iter::once(command.get_program())
        .chain(command.get_args())
        .collect();
    spawn_session(command.get_program(), &argv, &environment(command), stdio)
}

/// Starts `program` with `argv` (whose first entry is the name the program sees for itself) and
/// exactly the environment `env`, as [`spawn_detached`] describes: in a new session and process
/// group whose id is the returned pid, with `stdio` as its only descriptors, and in this process's
/// working directory. `program` must be a path; `PATH` is not searched.
///
/// `env`'s values may include a secret, such as an API key account's key (#118). This function's
/// own copies of them (the `NAME=value` pair it builds and the `CString` it turns that into) are
/// zeroized once `posix_spawn` has read them, whether or not it succeeds; `env` itself is the
/// caller's, and dropping it is the caller's job (`backend::process::Environment` does this for
/// the copy it holds).
///
/// # Errors
///
/// If an argument or environment variable contains a NUL byte, or `posix_spawn` fails.
pub(crate) fn spawn_session(
    program: &OsStr,
    argv: &[&OsStr],
    env: &BTreeMap<OsString, OsString>,
    stdio: Stdio<'_>,
) -> io::Result<Pid> {
    let c_args = argv.iter().map(c_string).collect::<io::Result<Vec<_>>>()?;
    let c_env = env
        .iter()
        .map(|(key, value)| {
            let mut pair = key.clone();
            pair.push("=");
            pair.push(value);
            let result = c_string(&pair);
            pair.into_encoded_bytes().zeroize();
            result
        })
        .collect::<io::Result<Vec<_>>>()?;

    // Copies above 2, so that setting up 0 to 2 in the child can't overwrite a source.
    let mut sources = Vec::with_capacity(3);
    for fd in [stdio.stdin, stdio.stdout, stdio.stderr] {
        sources.push(rustix::io::fcntl_dupfd_cloexec(fd, 3)?);
    }
    let mut actions = PosixSpawnFileActions::init()?;
    for (target, source) in (0..).zip(&sources) {
        actions.add_dup2(source.as_raw_fd(), target)?;
    }

    let mut attr = PosixSpawnAttr::init()?;
    attr.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF
            | PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK
            | PosixSpawnFlags::from_bits_retain(
                POSIX_SPAWN_SETSID | libc::POSIX_SPAWN_CLOEXEC_DEFAULT,
            ),
    )?;
    attr.set_sigdefault(&SigSet::all())?;
    attr.set_sigmask(&SigSet::empty())?;

    let spawned = posix_spawn(program, &actions, &attr, &c_args, &c_env);
    for entry in c_env {
        entry.into_bytes_with_nul().zeroize();
    }
    let pid = spawned?;
    Pid::from_raw(pid.as_raw()).ok_or_else(|| io::Error::other("posix_spawn returned pid 0"))
}

/// This process's environment with `command`'s changes applied.
fn environment(command: &Command) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                env.insert(key.to_owned(), value.to_owned());
            }
            None => {
                env.remove(key);
            }
        }
    }
    env
}

fn c_string(text: impl AsRef<OsStr>) -> io::Result<CString> {
    let text = text.as_ref();
    CString::new(text.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} contains a NUL byte", text.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsFd;
    use std::process::Command;

    use rustix::process::{WaitOptions, waitpid};

    use super::{Stdio, spawn_detached};

    #[test]
    fn the_child_gets_its_stdio_arguments_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in");
        fs::write(&input, "hello\n").unwrap();
        let stdin = File::open(&input).unwrap();
        let output = dir.path().join("out");
        let stdout = File::create(&output).unwrap();

        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "read line; echo \"$line $0 $WISPD_TEST\"; echo err >&2",
            ])
            .arg("arg")
            .env("WISPD_TEST", "set");
        let pid = spawn_detached(
            &command,
            Stdio {
                stdin: stdin.as_fd(),
                stdout: stdout.as_fd(),
                stderr: stdout.as_fd(),
            },
        )
        .unwrap();
        let (_, status) = waitpid(Some(pid), WaitOptions::empty()).unwrap().unwrap();
        assert_eq!(status.exit_status(), Some(0));
        assert_eq!(fs::read_to_string(&output).unwrap(), "hello arg set\nerr\n");
    }
}
