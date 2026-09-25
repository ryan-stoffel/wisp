# 0010: How `wispd attach` starts wispd, and its exit codes

- Status: accepted
- Date: 2026-09-24
- Issue: #60

## Context

`wispd attach` (#60) is how the editor reaches wispd, on the same Mac (#63) and on a host over SSH (#64). 0007 settles the basics:

- attach bridges stdio to the socket byte for byte.
- When wispd isn't running, attach starts it through the LaunchAgent (#61) if one is installed.
- Otherwise it starts `serve` in a new session, with stdio going to the log.

Several other issues build on the details:

- The editor needs to know from attach's exit status why a connection never happened.
- #61 has to install the LaunchAgent where attach looks for it, and #71 has to restart that same service.
- A wispd that attach starts from an SSH session must keep nothing of that session open. That includes the descriptors #86 is about.

## Decision

### Connecting

- attach finds the data folder and the socket with `DataDir`, the same way `serve` does (0009).
- `ENOENT` or `ECONNREFUSED` means nothing accepts connections yet, so attach starts wispd.
  - Any other error fails at once, because starting wispd can't fix it. Examples are `EACCES`, and `ENOTSOCK` for a file that isn't a socket.
- attach then retries with backoff, starting at 10 ms and doubling up to 500 ms, until `--connect-timeout` passes.
  - The timeout is 10 s by default, which is the editor's liveness window (0007). It can be at most a day.
- attach never stops or signals a wispd, including one it started.
- Once connected, attach waits for the last `serve` it started on a thread of its own. A `serve` that lost the lock to another therefore doesn't stay a zombie for as long as attach runs.

### Starting wispd

- **The LaunchAgent:**
  - Label: `io.github.ryan-stoffel.wisp.wispd`, which is 0006's bundle id plus `.wispd`. It is `wispd::service::DEFAULT_LABEL` (#61), which attach reuses.
  - Plist: `~/Library/LaunchAgents/io.github.ryan-stoffel.wisp.wispd.plist`.
  - Data folder: the agent under this label serves the default data folder. `wispd service install` refuses this label for any other folder. Another folder needs its own label, given with `--label`, which attach never starts.
  - Domain: `gui/<uid>`, the user's GUI session, where the Keychain is reachable (0004).
  - attach uses the LaunchAgent only for the default data folder, and only when the plist exists. It runs `launchctl kickstart gui/<uid>/io.github.ryan-stoffel.wisp.wispd`.
  - attach waits for `launchctl` until its connect deadline at most, then kills it.
  - If `kickstart` fails or runs out of time, attach says so on stderr and starts `serve` itself. On a host where nobody has logged in yet, `gui/<uid>` doesn't exist, so this is how attach reaches wispd there. #102 covers the handover after the first login.
- **Otherwise,** attach runs its own executable's `serve` with `posix_spawn`, using `nix`'s safe wrapper:
  - `POSIX_SPAWN_SETSID` gives `serve` a new session, so the terminal or SSH session that started it can't send it a hangup.
  - `POSIX_SPAWN_CLOEXEC_DEFAULT` gives it exactly three descriptors:
    - stdin on `/dev/null`.
    - stdout and stderr appended to `logs/wispd.log`.
    - Nothing else. That includes descriptors attach inherited without close-on-exec, and ones another thread just opened (#86). A `serve` that held one of attach's pipes would keep the SSH session open.
  - Every signal is at its default action and none is blocked.
  - `WISPD_DATA_DIR` is set through `DataDir::command`.
  - Before attach opens the log, it checks the data folder with `prepare_data_dir`, as `serve` would.
  - `serve` keeps attach's working directory until it has resolved its data folder, then changes to `/`, so it keeps no folder or volume busy.
- **When the `serve` that attach started stops:**
  - If it exits 3, attach starts another at the next retry. That waits out another instance's shutdown (0009).
  - If it stops any other way, the wait ends, and the error quotes the last line that `serve` logged.

### Bridging

- stdin goes to the socket, and the socket goes to stdout, byte for byte.
- stdout carries nothing else. attach's own messages go to stderr as `wispd attach: ...`.
- When stdin ends, attach shuts down the socket's write side and keeps copying until wispd closes. This is 0007's half-close.
- attach stops when wispd closes the connection, when stdout closes, or on SIGHUP. It then exits with `std::process::exit`.

### Exit codes of `attach`

| Code | Meaning |
| --- | --- |
| 0 | The connection ended: wispd closed it, stdout closed, or a SIGHUP arrived |
| 1 | An I/O error other than a close, after connecting |
| 2 | A usage error |
| 4 | Never reached wispd. It couldn't be started, the `serve` that attach started stopped, or the timeout passed |

Code 4 differs from the remote shell's 127 (`wispd` not found) and ssh's own 255.

## Alternatives

| Alternative | Why it lost |
| --- | --- |
| `std::process::Command`, with `serve` calling `setsid()` behind a hidden flag | `CommandExt::setsid` is unstable in Rust 1.98. `Command` also passes on every descriptor that attach inherited without close-on-exec. |
| `Command::process_group(0)` | A new process group isn't a new session. |
| `posix_spawn` or `setsid` through `libc` | The workspace denies unsafe code. `nix`'s safe wrapper makes the same call. |
| `launchctl submit` | It is deprecated, and it leaves a launchd job behind. |

## Consequences

- **#61:** it installs the LaunchAgent under the label, plist path, and domain above. `service install` enforces that the default label serves the default data folder.
- **#71:** it can restart the same service with `launchctl kickstart -k`.
- **#63 and #64:** they treat exit 4 as "wispd can't be reached on that machine", and show attach's stderr.
- **Environment:** a `serve` that attach starts over SSH inherits that session's environment, including its short `PATH`. #96 decides what agents get. On a host, the LaunchAgent stays the recommended setup, also because a process started from SSH may not reach the Keychain (0004).
- **Dependencies:** wispd depends on `nix` for `posix_spawn`, next to `rustix`, which has no spawn API.
