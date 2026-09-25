# 0012: Routing a task to a backend and account

- Status: accepted
- Date: 2026-09-25
- Issue: #119

## Context

M2 needs a task to resolve to a backend and an account: named outright, or the task's role's
stored default. #114 (detecting installed CLIs and their sign-in state) has not shipped yet, so
wisp has no store-backed notion of a "subscription account" distinct from the backend that runs
it. #119 still has to let a task name one.

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
  I/O) and `start()` (reads the credential and starts the run). The coordinator role always gets
  `ToolPolicy::NoWrite`, regardless of what was requested.
- **A `BackendRegistry` maps one backend per `Provider`.** A backend takes both a subscription
  login and a key account for its provider already (0004's Claude backend), so fallback never
  needs a second backend, only a different `Credential` on the same one.
- **Fallback has no separate "which key account" setting.** `KeyAccounts::fallback_for(provider)`
  answers with any key account configured for that provider; M2 does not need to let the user pick
  among several. `start()` retries at most once, only from a subscription's `notSignedIn` or
  `rateLimited` failure, and marks the switch with a new `backend::Event::AccountFallback`, which
  is the fallback run's first event.
- **The no-write policy's second check — `git status --porcelain` after a coordinator's turn — is
  `routing::check_no_write_policy`,** not backend-specific, since it doesn't depend on which CLI
  ran. It returns a `Failure` for the caller to end the run with; #156's runner calls it after each
  `Event::TurnFinished` on a coordinator run.

## Consequences

- #156 (the M3 runner) calls `routing::resolve` and `routing::start`, and owns turning
  `Event::AccountFallback` and a `check_no_write_policy` violation into `agent/*` protocol events
  and stopping the run; #119 only produces the values, since the runner and its store tables don't
  exist yet.
- `start()`'s returned `Started::run` is always the first attempt's control handle. A fallback
  starts a second process the caller has no direct handle to; #156, which already has to keep a
  run's handle by `runId`, re-points it when it sees `Event::AccountFallback`.
- #114 lands a real, listable subscription account identity later. `AccountChoice::Subscription`
  keeps working as today's one-account-per-backend meaning until that issue changes it.
