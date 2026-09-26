# 0012: Routing a task to a backend and account

- Status: accepted
- Date: 2026-09-25
- Issue: #119

## Context

M2 needs a task to resolve to a backend and an account: named outright, or the task's role's
stored default. #114 (detecting installed CLIs and their sign-in state) had not shipped when this
was written, so wisp had no store-backed notion of a "subscription account" distinct from the
backend that runs it, and #119 had to let a task name one anyway. #114 has since shipped, but
wiring `accounts/defaults/set` to its detection and giving a subscription account a real identity
is #170's job, not this one's.

## Decision

- **`AccountChoice`** (`crates/wisp-protocol/src/defaults.rs`) is either `Subscription { backend }`
  — the user's own login in the named backend, such as `claude` — or `Key { id }`, a key account
  from `accounts/keys/*` (#117). A subscription account has no id of its own yet; it is named by
  the backend that runs it, since M2 ships one account per backend. #114 can add a real
  `AccountId`-style identity for a second login on the same machine later without changing this
  shape: `Subscription` gains an optional field, defaulting to today's one-per-backend meaning.
- **Per-host defaults** live in a new `role_defaults` store table (`crates/wisp-store`), one row
  per role, set and read over `accounts/defaults/get` and `accounts/defaults/set`
  (`daemon/src/methods/defaults.rs`). Setting `account: null` clears a role's default.
- **`daemon/src/routing.rs`** is the decision engine `resolve()` (account + backend, in-memory, no
  I/O) and `start()` (reads the credential and starts the run). `Resolved`'s fields are private:
  `resolve()` is the only place that decides a coordinator's policy, `start()` re-applies
  `ToolPolicy::NoWrite` for `Role::Coordinator` from `Resolved::role()` regardless of what
  `Resolved::policy()` already says, and nothing between the two calls can substitute a
  `workspace-write` policy for it.
- **A `BackendRegistry` maps one backend per `Provider`.** A backend takes both a subscription
  login and a key account for its provider already (0004's Claude backend), so fallback never
  needs a second backend, only a different `Credential` on the same one.
- **Fallback has no separate "which key account" setting.** `KeyAccounts::fallback_for(provider)`
  answers with any key account configured for that provider; M2 does not need to let the user pick
  among several. `start()` retries at most once, only from a subscription's `notSignedIn` or
  `rateLimited` failure, and marks the switch with `backend::Event::AccountFallback`, the fallback
  run's first event. That event names both `from_account` and `to_account`: everything after it,
  including that run's own `Usage` and `RateLimit` events, is charged to `to_account`. `start()`
  returns a `Run` handle (`routing::FallbackRun`) that forwards to whichever attempt is actually
  running, swapped in before `AccountFallback` is emitted, so `cancel` and `send` reach the running
  process even after a fallback, and the outer `EventSink`'s running usage total is reset to the
  fallback's own baseline at the same point, so `Finished.usage_totals` reports only that account's
  session, not both attempts summed together.
- **The no-write policy's second check is `routing::snapshot` and `routing::check`,** not
  backend-specific, since it doesn't depend on which CLI ran, and not a single "is the tree dirty"
  check, since a project repo is often already dirty while the user works (0004: "if `git status`
  changes during its turn"). The caller takes a `snapshot` before the turn and `check`s it after;
  each is a hash of `git status --porcelain=v1 -z --untracked-files=all`, `git diff HEAD --binary`,
  and every untracked file's contents, so a further edit to an already-modified file, or a new
  untracked file, both still count as a change even though the tree was never clean.
- **`accounts/defaults/set` validates before it writes.** A `Key` choice must be a real row in
  `accounts`; a `Subscription` choice's backend must be in a fixed list of vendor CLIs 0004 commits
  to (`claude`, `codex`, `cursor`) until #170 wires this check to #114's real detection instead.
  Either failure is `invalidParams`, naming the account or backend.
- **The post-turn check has a known blind spot: gitignored writes.** `routing::snapshot` and
  `routing::check` hash `git status` and `git diff`, so a write to a file `.gitignore` excludes —
  `.env`, `.vscode/`, build output — passes uncaught. 0004 accepts this ("The check misses writes
  outside the repo and to ignored files"): the tool allowlist, not this check, is the main guard,
  and hashing every ignored file (`node_modules`, `target`, and the like) would be too expensive to
  run after every turn. A caller of `check` sees only what this check actually covers, not "any
  write."

## Consequences

- #156 (the M3 runner, workers only) calls `routing::resolve` and `routing::start` for a worker's
  `workspace-write` run, maps `Event::AccountFallback` to an `agent/*` notification, and charges
  usage after it to `to_account`. It does not call `routing::snapshot`/`routing::check`: those are
  a coordinator's job, and #156 doesn't run coordinators.
- No M4 task issue exists yet to call `routing::resolve`/`start` for the coordinator, or
  `routing::snapshot` before a coordinator's turn and `routing::check` after it (stopping the run
  and reporting `policyViolation` on a violation). #24 (Epic: M4: Coordinator) lists "Coordinator
  planning loop" as a planned task; that is where this belongs once M4's task issues are filed.
- #170 gives subscription accounts a real, listable identity, validates `accounts/defaults/set`'s
  `Subscription` choices against #114's `DetectedCli` instead of the fixed `claude`/`codex`/`cursor`
  list, and migrates `role_defaults` rows and usage rows already keyed by a backend name such as
  `"claude"` to that real identity rather than orphaning them. `AccountChoice::Subscription` keeps
  working as today's one-account-per-backend meaning until #170 lands.
