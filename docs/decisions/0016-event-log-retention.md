# 0016: Retention for the stored event log and its in-memory replay window

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

Events with no `run_id` — `project.created` and `context.changed` — are pruned to the newest `host_event_retention` (default 10,000, the same figure as `event_retention`, since these events are far rarer than a run's `agent.output`). `EventLog::append` prunes after storing any such event, calling `Store::prune_host_events`, which deletes rows outside that window with `run_id IS NULL`. The existing `events_run (run_id, seq)` index already serves that `DELETE` (SQLite uses it for `run_id IS NULL ORDER BY seq`, confirmed with `EXPLAIN QUERY PLAN` on a 500k-row table), so no new index is needed. The delete itself finds its cutoff with `ORDER BY seq DESC LIMIT 1 OFFSET host_event_retention` rather than collecting the newest `host_event_retention` rows into a list to exclude — about 0.1 ms against 1.5–3 ms measured the other way, which matters here since it runs inside `EventLog`'s single mutex, serialized with every append and every subscriber's replay.

Count over age: age needs a wall-clock cutoff and a periodic sweep to catch events that stop arriving; count is a simple bound on worst-case table size and needs no extra scheduling, since it runs opportunistically on the appends that could grow the table.

### `host_event_retention` is always at least `event_retention`

`EventLog::with` clamps it, since a smaller value would be actively unsafe, not just a worse default: a restart only ever reloads the newest `event_retention` events (`EventLog::load`), and by pigeonhole every host or project event among them is necessarily among the newest `event_retention` host and project events too. Keeping at least that many in the table means a reload can never be missing one it expects. Without the clamp, a small enough `host_event_retention` prunes a host event that a reload still needed, leaving a hole in the reloaded window that nothing detects: `check`/`next` only compare against the oldest loaded `seq` and already tolerate an interior gap (the same one a failed insert leaves), so they would replay past it in silence instead of asking the client to resync.

### `resyncRequired` stays correct

`events/subscribe` and its cursors (`EventLog::check`/`next`) only ever read the in-memory window, never the database, so pruning a row from the table can never change what a live subscriber sees. The invariant above is what keeps a *restart* safe too: because a reload can't be missing a host event it needed, the only gaps a reload ever has are ones from events genuinely outside the newest `event_retention` (ordinary retention) or from a failed insert (already tolerated) — never from pruning cutting into the middle of what should have reloaded. A resubscribe from a `seq` those genuinely-gone events would have answered still correctly gets `resyncRequired`.

### The in-memory window is also bounded by bytes

`EventLog` now takes `event_retention_bytes` (default 64 MiB) alongside `event_retention`. Both `append` and construction (fresh, and on every reload) evict from the front once either bound is passed, always leaving at least one event. `Store::latest_events` also stops reading rows once the byte budget is spent, so a restart never materializes more of the table than it will keep. 64 MiB comfortably holds `event_retention`'s 10,000 events at M1's sizes, while capping a burst of large `agent.output` batches well short of holding all 10,000 of them in memory at once — including right after a restart, not only once enough appends have happened to evict down to it.

## Alternatives

| Alternative | Why it lost |
| --- | --- |
| Prune run events too, by a guessed retention (e.g. keep the last 50 runs) | No run-removal feature exists to anchor it to; `agent/list` and `agent/events` would need to agree on what "removed" means for a run row that's still shown. Deferred to #207. |
| Age-based pruning for host and project events | Needs a wall-clock cutoff and its own periodic sweep; a count-based prune runs for free on the appends that grow the table and bounds worst-case size just as well for events this rare. |
| One global row cap across all events (host, project, and run) | Would silently prune a long run's own `agent.output` history, breaking `agent/events`' promise to rebuild a run's whole transcript. |
| A partial index on `events (seq) WHERE run_id IS NULL` for the prune | Measured no benefit: `events_run (run_id, seq)` already serves `run_id IS NULL ORDER BY seq` on its own, so the extra index would only cost a write on every host or project insert. |

## Consequences

- `wispd.sqlite3`'s growth from `project.created` and `context.changed` is now bounded; growth from agent runs' own events is not, until #207.
- `host_event_retention` can't be configured below `event_retention` — it is clamped up rather than rejected, so a low value silently becomes `event_retention` instead of erroring.
- The in-memory replay window's peak size is now bounded by `event_retention_bytes` on a fresh log and after a restart alike, not only once enough appends have evicted down to it.
