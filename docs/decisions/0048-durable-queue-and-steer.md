# 0048: Waiting messages are stored, and a steer goes into the running turn

- Status: accepted; supersedes in part [0014](0014-agent-runs.md) (waiting messages lived in the run's actor only, and a message sent during a turn went to the CLI at once)
- Date: 2026-10-03
- Issue: PLX-370, part of PLX-368

## Context

0014 handed a message sent during a turn straight to the run's CLI, and kept only messages that change the model, options, or account in the actor's memory until the CLI exited. A plxd restart dropped those with `followUpDropped`, and a client couldn't list, edit, reorder, or cancel any of them. There was no way to choose between putting a message into the turn that is running now (a steer) and sending it after.

Each CLI handles a mid-turn message differently. On 2026-10-03, on macOS 27.0 with Ryan's own logins, each ran "run `sleep 8`, then reply DONE" and got "reply BANANA instead" while the command ran:

| CLI | What it did with the mid-turn message |
| --- | --- |
| Claude Code 2.1.288, `haiku`, stream-json | A user message on stdin is reported `command_lifecycle queued`, then `started` right after the running tool's result, inside the same turn. One `result`, "BANANA", names both uuids. So stdin is a native steer, and 0014's mid-turn follow-ups were already steers. |
| codex app-server 0.160.0, `gpt-6-luna` | `turn/steer {threadId, expectedTurnId, input}` answered `{turnId}` of the running turn. The message joined that turn after the command, which ended "BANANA". |
| Cursor Agent 2026.10.01-14929f9, `composer-2.5`, `agent acp` | ACP has no steer. A second `session/prompt` mid-turn makes Cursor end the first with `stopReason: cancelled` and run the new one. `session/cancel`, then `session/prompt`, does the same explicitly. |

## Decision

### The queue

- A message sent while the run's CLI works on a turn waits in the run's queue until the turn ends, then goes to the same CLI as its next turn. The actor counts the turns it gave the CLI that haven't finished, and sends the next waiting message when that count is 0.
- While a waiting message can go to the live CLI, the actor holds it (`Run::hold`), so the Claude, Codex, and Cursor drivers don't close stdin when the CLI is idle. A message that changes the model, options, or account still waits for the CLI to exit and starts a new one (0014), with every message after it, and sets no hold.
- A message the live CLI took but drops as it exits goes back to the front of the queue for the next CLI.
- The queue is stored in the store's `queued` table (migration 25), with each message's images, attached threads, options, and account. When plxd stops it keeps the queue. When plxd starts, every run with stored messages gets its actor, which resumes the session with the first and sends the rest in order. `agent/cancel` still drops every waiting message with `followUpDropped`.
- `agent/send` answers with the store's error when it can't store a message that would wait, and the message isn't queued.
- A waiting message is sent at least once. The CLI gets it before its stored row is deleted, as a turn is sent before `record_turn` stores it, so a crash between the two resends it once after a restart. Sending a message twice is better than losing one.
- `agent/send` stays idempotent on `turnId` for a waiting message. A waiting message's id in `queue/*` is its `turnId`.

### Steer

- `agent/send`'s `delivery: steer` sends the message into the running turn, ahead of anything waiting. A steer can't change the model, options, or account (`unsupportedOption`). With no CLI running, it resumes the session at once.
- Claude Code gets it on stdin. Codex gets `turn/steer` with the running turn's id, and the steer's turn ends with that turn; if Codex refuses it, or no turn runs yet, it is the next turn. Cursor gets `session/cancel` for the turn in flight, then the steer as the next `session/prompt`.
- A backend that takes no messages while it runs is interrupted instead: plxd cancels the CLI and resumes the session with the message. The run's transcript shows the cancelled CLI.

### Protocol, behind the `queue` capability

| Method | Params | Result |
| --- | --- | --- |
| `queue/list` | `{runId}` | `{messages}` |
| `queue/edit` | `{runId, id, text}` | `{messages}`, the message's images kept |
| `queue/reorder` | `{runId, ids}` | `{messages}`; `ids` must list every waiting message once (`invalidParams`) |
| `queue/cancel` | `{runId, id}` | `{messages}`; the message is logged `followUpDropped` |
| `queue/steer` | `{runId, id}` | `{messages}`; the message leaves the queue as `delivery: steer` sends it |

- `messages` are `{id, text, images, threads}`, `images` being a count and `threads` the attached threads' run ids, first to be sent first. An unknown id is `queuedMessageNotFound`.
- Every change, a message sent or dropped included, appends `queue.updated {runId, messages}` on the run's project scope, stored with the run's events.
- `agent/send` takes `delivery` only from a client that sees `queue`; an older plxd would queue a steer.

## Consequences

- A queued message to a running Claude or Codex thread runs in the same process instead of a new one, as before, but after the turn rather than inside it. The coordinator's `message_agent` and `agent/requestChanges` queue.
- The app can show, edit, reorder, cancel, and steer waiting messages (PLX-376), and PLX-368's `thread_send` takes queue or steer.
- A steer on Cursor costs the turn in progress: Cursor reran `sleep 8` in the steered turn.

## Evidence

The same day, `plxd serve` with a fresh data folder ran a thread on each CLI over `plxd attach`, in Bypass Permissions, with "run `sleep 8`, then reply DONE". During the command a client queued "reply CHERRY" and then steered "reply BANANA instead":

| Thread | Transcript |
| --- | --- |
| Claude Code, `haiku` | The steer's turn started inside the prompt's; one result, "BANANA", ended both; then the queued turn, "CHERRY", in the same process |
| Codex, `gpt-6-luna` | `turn/steer` joined the running turn, "BANANA" ended both; the queued turn, "CHERRY", ran next in the same app-server |
| Cursor, `composer-2.5` | The prompt's turn ended cancelled; the steer ran as its own turn, reran the command, and said "BANANA"; then "CHERRY" |

Each ended with one `agent.finished`, `completed`. `daemon/tests/server/queue.rs` covers a restart with waiting messages, reorder, edit, cancel, steer, and the interrupt fallback on the fake backend. `codex/fixtures/app-server-steer.jsonl` and `cursor/fixtures/steer.jsonl` replay the real shapes through each driver, and `claude/fixtures/held.jsonl` checks that a held CLI keeps stdin open.
