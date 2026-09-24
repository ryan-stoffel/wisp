# 0009: wispd's data folder, and projects without a host

- Status: accepted
- Date: 2026-09-24
- Issue: #59

## Context

`wispd serve` (#59) is the first code that writes to the data folder that 0006 names and 0007 secures. `wispd attach` (#60), the LaunchAgent (#61), and the end-to-end tests (#66) have to find the same files and read the same exit codes.

#58's store also gives every project a `host` column, which the protocol's `Project` (#57) has no field for. #82 flagged the mismatch.

## Decision

### Files

The editor shares the data folder, `~/Library/Application Support/wisp` (0006), so everything wispd owns starts with `wispd` or sits in `logs/`:

- `wispd.sock`, or the fallback path from 0007 when that path would be longer than 103 bytes. The fallback's `<hash>` is taken over the data folder's absolute path, with `.` components and trailing slashes dropped and symlinks left unresolved.
- `wispd.lock`, which `serve` holds with `flock`. It contains that process's pid.
  - At shutdown, `serve` removes the file before it lets go of the lock.
  - After locking, a starting `serve` checks that the file it locked is still the one at the path, so the removal can't let two instances in.
- `wispd.sqlite3`, the store, with SQLite's `-wal` and `-shm` files.
- `logs/wispd.log`, which `serve` appends to. It isn't rotated yet (#85).

### Overrides

- The data folder is `--data-dir`, then `WISPD_DATA_DIR`, then the default.
- Every subcommand that reaches the socket resolves the folder and the socket path with `wispd::paths::DataDir`, so `serve` and `attach` always agree.
- Tests use the override to stay out of the real folder.

### Logging

- The level is `--log-level`, then `WISPD_LOG`, then `info`. It is a level, or `target=level` pairs with an optional default, such as `wispd=debug,warn`. A word that isn't a level is an error.
- Lines go to `logs/wispd.log`, and also to stderr when stderr is a terminal. So `attach` and the LaunchAgent can send `serve`'s stdout and stderr to the same file, which catches a panic without writing every line twice.

### Exit codes of `serve`

| Code | Meaning |
| --- | --- |
| 0 | Stopped cleanly after SIGTERM or SIGINT |
| 1 | Couldn't start, or failed |
| 2 | A usage error |
| 3 | Another `serve` already runs for this data folder |

When two `attach` processes race to start `serve`, the one that gets 3 can just connect.

### Shutdown

SIGTERM and SIGINT stop accepting, remove the socket, and let in-flight requests finish for up to 10 s before cancelling them. A second signal stops waiting.

### A missing store

If the store can't be opened, for example after a downgrade across a schema change, `serve` runs anyway. `host/health` reports `store: unavailable` and project methods fail with -32603, so the editor can show what is wrong instead of failing to connect.

### M1's event log

The log is in memory, as 0007 allows for M1: a new `logId` on every start, and the last 10,000 events kept for replay.

### Projects have no host field

Each wispd serves one user on one machine (0007), so every project in its store is on that host, and the editor knows the host from the connection it opened. Migration 2 drops `projects.host`, and `wisp-store`'s `Project` and `ProjectFields` lose the field.

| Alternative | Why it lost |
| --- | --- |
| Store the hostname | After the Mac is renamed, a retried `project/create` would fail with `idConflict`, and old rows would name the old host. |
| Store a constant | A column that means nothing. |
| Add `host` to the protocol | It could only repeat what the connection already says. |

## Consequences

- #60's `attach` finds the socket with `DataDir::resolve(...)?.socket_path()`, and starts `serve` with its stdio on `logs/wispd.log`.
- #61's LaunchAgent points `StandardOutPath` and `StandardErrorPath` at `logs/wispd.log`.
- #66's end-to-end tests set `WISPD_DATA_DIR` to a short temporary folder, which keeps the socket path under 103 bytes.
- If a project ever needs to name another machine, it gets a new field with that meaning. M5's local runner is chosen per run, not per project.
