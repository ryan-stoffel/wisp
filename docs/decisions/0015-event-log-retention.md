# 0015: Retention for the stored event log and its in-memory replay window

- Status: accepted
- Date: 2026-09-25
- Issue: #187

## Context

0014 moved the event log into SQLite and left a note: "The table isn't pruned yet (#187)." Two bounds were missing:

- The `events` table grows forever. Every `project.created`, `context.changed`, and `agent.*` event stays, so `wispd.sqlite3` grows with every agent run.
- `EventLog`'s in-memory window, which `events/subscribe` replays from, was bounded only by count (`event_retention`, 10,000). A run's `agent.output` batches can reach about 256 KiB each (0007's per-tool-output cap is 32 KiB), so a long run with large outputs could hold far more memory than `event_retention` was sized for when M1's events were all small.

## Decision

### Run events stay while their run does

An agent run's events (`agent.started`, `agent.updated`, `agent.output`, `agent.accountFallback`, `agent.finished`, `agent.diffReady`) are never pruned by the event log itself. They stay in the table for as long as the run's row does in `runs`. Nothing removes a run's row today, so in practice these events are not pruned by this decision — that's deliberate. `agent/events` pages a run's whole history from the table (0014), and guessing at a run-history retention policy (keep the last N finished runs? by age? per project?) without a real run-removal feature to hang it on would be arbitrary. #207 tracks pruning old finished runs (and their events) once that's needed.

### Host and project events are pruned by count

Events with no `run_id` — `project.created` and `context.changed` — are pruned to the newest `host_event_retention` (default 10,000, the same figure as `event_retention`, since these events are far rarer than a run's `agent.output`). `EventLog::append` prunes after storing any such event, calling `Store::prune_host_events`, which deletes rows outside that window with `run_id IS NULL`. A partial index (`events (seq) WHERE run_id IS NULL`, migration 8) keeps that `DELETE` cheap regardless of how many run events sit in the table.

Count over age: age needs a wall-clock cutoff and a periodic sweep to catch events that stop arriving; count is a simple bound on worst-case table size and needs no extra scheduling, since it runs opportunistically on the appends that could grow the table.

### `resyncRequired` stays correct

`events/subscribe` and its cursors (`EventLog::check`/`next`) only ever read the in-memory window, never the database. Pruning a row from the table can't change what a live subscriber sees, and can't make `resyncRequired` fire when it shouldn't. On restart, `EventLog::load` reloads the newest events from the table; pruning simply means some of the reloaded window's `seq`s are missing, which the log already tolerates (a failed insert leaves the same kind of gap). A resubscribe from a `seq` that only the pruned rows would have answered gets `resyncRequired`, exactly as it already does for a `seq` past `event_retention`'s in-memory window.

### The in-memory window is also bounded by bytes

`EventLog` now takes `event_retention_bytes` (default 64 MiB) alongside `event_retention`. Appending evicts from the front once either bound is passed, always leaving at least the event just appended. 64 MiB comfortably holds `event_retention`'s 10,000 events at M1's sizes, while capping a burst of large `agent.output` batches well short of holding all 10,000 of them in memory at once.

## Alternatives

| Alternative | Why it lost |
| --- | --- |
| Prune run events too, by a guessed retention (e.g. keep the last 50 runs) | No run-removal feature exists to anchor it to; `agent/list` and `agent/events` would need to agree on what "removed" means for a run row that's still shown. Deferred to #207. |
| Age-based pruning for host and project events | Needs a wall-clock cutoff and its own periodic sweep; a count-based prune runs for free on the appends that grow the table and bounds worst-case size just as well for events this rare. |
| One global row cap across all events (host, project, and run) | Would silently prune a long run's own `agent.output` history, breaking `agent/events`' promise to rebuild a run's whole transcript. |

## Consequences

- `wispd.sqlite3`'s growth from `project.created` and `context.changed` is now bounded; growth from agent runs' own events is not, until #207.
- A restart's replay window can now have gaps from pruning as well as from a failed insert; both are already handled the same way.
- The in-memory replay window's peak size is now predictable (about `event_retention_bytes`) instead of scaling with `event_retention` times whatever the largest event happens to be.
