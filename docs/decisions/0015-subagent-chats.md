# 0015: Subagents are wisp.agent chats, and the Agents pill opens wisp's panel

- Status: accepted
- Date: 2026-09-25
- Issue: #105

## Context

0011 mapped Ryan's Agents pill onto upstream's subagent pill and `ISessionsService.openChat`, and left three things for #105 to find out: how the pill could present a card instead of a menu, what a subagent's chat needs from upstream's chat to take a message, and how its transcript survives a reconnect. 0014 gave the editor the runs (`agent/list`, `agent/events`, and the `agent.*` events). Building #105 on Code - OSS 1.139.0 answered all three. The M4 coordinator (#104) and the diff review (#157) depend on the answers.

## Decision

### Runs as chats

- Each run is a chat of its project's session, with the resource `wisp.agent:/<project>/<runId>`, `origin: { kind: Tool, parentChat: <coordinator> }`, and `interactivity: Full`. Its `status` and state text follow the run, and its `description` says where it runs. `IChat.changes` stays empty until the review (#157) can list the worktree's files (#194).
- `IWispAgentsService` lists each project's runs and subscribes to the project's events from the list's `seq`. It keeps every event of a run by `seq`, so a transcript loaded with `agent/events` and one built from live events never show an event twice. After a `resync`, it lists again and fetches what any loaded transcript missed, and only then subscribes.
- The `wisp.agent` chat session type builds each run's transcript from its events. The task and each message sent with `agent/send` are one request and response each. A turn's response stays open until the CLI ends or the next message starts. It closes with a card that says how the CLI ended, with **Review changes** (`wisp.reviewAgentChanges`, from #157, shown only once that command exists) and **Retry** (a new run of the same task on the same account). The card waits for the run's new status, since wispd reports the commit after `agent.finished`.

### A default chat agent is required to send

Upstream's `ChatService.sendRequest` rejects every request when no chat agent is the default for its location and mode, even one addressed to a session type's own agent. Without Copilot, wisp has none. So the `wisp.agent` chat agent registers as the default for agent-mode chat in the Chat location. It answers a request for any chat that isn't a subagent with an error.

- `chat.enabled` (`ChatContextKeys.enabled`) is now true in the Agents window, as it is upstream with Copilot.
- The coordinator can't send, since its type needs its own models and has none (M2, M4).
- When M4 lets the coordinator take messages, it registers its own agent for `wisp.project`. Either one agent routes both session types, or the default moves to the coordinator's. That choice belongs to #104.

### The pill and the panel

Four patches, all small:

| Patch | File | Lines |
| --- | --- | --- |
| `branding: call subagents Agents in the pill` | `sessionBackgroundActivitiesControl.ts` | 1 |
| `chat: show status and location in the Agents pill` | same | 19 added, 4 removed |
| `chat: present the Agents pill as a panel` | `workbench/browser/chatDropdownPill.ts` | 55 added, 5 removed |
| `chat: show the Agents pill by default` | `workbench/contrib/chat/common/sessionChatPills.ts` | 1 |

- **Presentation.** The pill's dropdown is the action widget, a menu anchored to the pill. It has no close button, no **More**, and no state column. 0011 expected the patch in `chatPills.ts`, but the dropdown lives in `chatDropdownPill.ts`. The patch adds `registerChatPillPanel(widgetId, panel)` there. A registered panel supplies the pill's summary (label, trailing marks, accessible name) and replaces the menu with its own view. wisp's overlay registers one for `sessionSubagents`. Its card over the bottom of the transcript is wisp's code, rendered through `IContextViewService`.
- **Default visibility.** Upstream hides the subagents pill until the user turns it on (`defaultHiddenKinds`). The Agents pill is how wisp shows subagents at all, so it shows by default. The user can still hide it from the pills' context menu.

### Until the coordinator exists

A developer command, **Wisp: Start Subagent**, starts a run on the active project. With no worker default (0012), it asks which signed-in CLI to use, and it saves nothing.

## Consequences

- #157's review plugs in by registering `wisp.reviewAgentChanges`. The card offers it from then on, with no change here.
- wispd logs a message's turn id but not its text, so a transcript rebuilt after a reload shows "A message sent to this agent" for messages this window didn't send in this session. #193 asks wispd to log the text.
- An upstream upgrade that changes how `chatDropdownPill.ts` builds its dropdown conflicts with the panel patch. The hook is small, and the panel itself is overlay code, which breaks at type-check instead.
- Upstream's own `sessionBackgroundActivitiesControl` tests expect "Subagents" and the agent icon. wisp doesn't run them.
