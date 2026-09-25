# wisp on the Agents window

- Status: design for M0 to M4, from #100. It supersedes [coordinator-chat.md](coordinator-chat.md), whose layout A Ryan replaced on #14.
- Decision: [0011](../decisions/0011-agents-window-baseline.md). Builds on 0004, 0005, and 0007.
- Upstream: the Agents window of Code - OSS 1.139.0 (`src/vs/sessions/`).

wisp's main UI is upstream's Agents window, with Copilot's parts removed and wisp's own sessions provider in their place. The sidebar lists projects, each with its coordinator and, nested under it, the subagents it started. The center shows the selected session's chat. The auxiliary bar shows the changes in each agent's worktree, and review opens a diff in the detail pane. The host is in the title bar, since the window has no status bar. Shared context takes the sidebar slot of upstream's AI Customizations. The editor window stays for quick edits.

![wisp on the Agents window, Dark Modern, annotated](agents-window/coordinator-notes.png)

## Mockups

All at 1440x900. The `-notes` images carry numbered annotations: teal for what ships in M0, purple for later milestones. The clean images are the same scenes without them.

| Scene | Dark | Light | Annotated |
| --- | --- | --- | --- |
| Coordinator session, M4 | [coordinator-dark.png](agents-window/coordinator-dark.png) | [coordinator-light.png](agents-window/coordinator-light.png) | [coordinator-notes.png](agents-window/coordinator-notes.png) |
| Subagent session, M3 | [subagent-dark.png](agents-window/subagent-dark.png) | [subagent-light.png](agents-window/subagent-light.png) | [subagent-notes.png](agents-window/subagent-notes.png) |
| Diff review of one worktree, M3 | [review-dark.png](agents-window/review-dark.png) | [review-light.png](agents-window/review-light.png) | [review-notes.png](agents-window/review-notes.png) |
| Host menu and a shared context file, M1 and M3 | [host-dark.png](agents-window/host-dark.png) | [host-light.png](agents-window/host-light.png) | [host-notes.png](agents-window/host-notes.png) |
| What M0 ships (#12) | [m0-dark.png](agents-window/m0-dark.png) | [m0-light.png](agents-window/m0-light.png) | [m0-notes.png](agents-window/m0-notes.png) |

Upstream's window as it is, with no Copilot, from the #100 experiment, is in [agents-window/upstream/](agents-window/upstream/):

| Image | What it shows |
| --- | --- |
| [1-sign-in-gate.png](agents-window/upstream/1-sign-in-gate.png) | First launch: "Sign in to use Agents", which wisp's app can't complete |
| [2-past-the-gate-dark.png](agents-window/upstream/2-past-the-gate-dark.png), [3-past-the-gate-light.png](agents-window/upstream/3-past-the-gate-light.png) | Past the gate: no session types, no models; Send does nothing |
| [4-workspace-picker.png](agents-window/upstream/4-workspace-picker.png) | The workspace picker: Local, GitHub, Remote |
| [5-with-10s-exclusions.png](agents-window/upstream/5-with-10s-exclusions.png) | The window with #10's exclusions and AI features off |

## Annotations

| # | Surface | How it is built | Milestone | Issue |
| --- | --- | --- | --- | --- |
| 1 | Projects: workspace sections of the Sessions list, each with its host | `ISessionWorkspace` from `WispSessionsProvider`; **New project** is a browse action | M1 | #104 |
| 2 | Coordinator: the first row of each project | Session type `wisp.coordinator`, one per project | M1 shell, M4 live | #104 |
| 3 | Subagents, nested under their coordinator, with status | Session type `wisp.agent`, `createdBySession`, `status`, `description` | M3 | #105 |
| 4 | Host status in the title bar, and its menu | A title bar item and a Quick Pick; `remoteConnectionStatus` on sessions | M0 static, M1 live | #12, #65, #104 |
| 5 | Coordinator transcript: plan card, events, result and error cards | Upstream's chat widget, fed by wisp's `IChatSession` | M4 | #104, M4 issues |
| 6 | Account and model picker | The provider's `getModelsSnapshot` | M2 | M2 issues |
| 7 | Changes, one group per agent worktree | `changes` and `changesets`, shown by upstream's Changes view | M3 | #105 |
| 8 | Shared context | A `wisp.sharedContext` view and a `wisp-context:` file system provider | M3 | #106 |
| 9 | Subagent transcript, read-only | Chat interactivity read-only; rebuilt from `agent/output` | M3 | #105 |
| 10 | No composer on a subagent | A bar: **Stop agent**, **Ask the coordinator** | M3 | #105 |
| 11 | Diff review in the detail pane | Upstream's Changes editor in the single-pane detail layout | M3 | #105 |
| 12 | Review bar: **Accept**, **Request changes** | Menu contributions on the changeset; #68 decides Accept | M3 | #105, #68 |
| 13 | Copilot's parts removed | Exclusion list and registry filter | M0 | #103 |
| 14 | wisp's provider registered, with no sessions yet | `WispSessionsProvider` shell | M0 | #12 |
| 15 | No host connected | A Custom View Grid view (`AbstractCustomView`) | M0 | #12 |

## Layout

Upstream owns the layout (`src/vs/sessions/LAYOUT.md`), and wisp changes none of it.

| Part | Upstream | wisp |
| --- | --- | --- |
| Title bar, 35 px | Sidebar toggle, the session title, layout toggles, the Copilot account menu | The same, with the account menu replaced by the host item |
| Sidebar, 280 px | Sessions list, then AI Customizations | Sessions list grouped by project, then Shared context. The mocks label the list's button **New project**; upstream's label is **New**, and renaming it is question 4 on #100. |
| Sessions part | The active session's chat, or several side by side | The same: coordinator or subagent |
| Auxiliary bar, 300 px | Changes, Files | The same, per agent worktree |
| Detail pane | Editor and details next to the session | Diff review and shared context files |
| Panel | Terminal | The same, in the session's worktree on the host (later) |
| Custom View Grid | Full-surface contributed views | The no-host state (M0); a project overview later if wanted |

There is no activity bar and no status bar. The M1 host status therefore moves from the status bar (#65's original plan) to the title bar.

## States

The chat states, copy, and error table in [coordinator-chat.md](coordinator-chat.md) carry over, with two changes:

- **Where they render.** The empty and error states render inside upstream's session surface, or, for connection-level problems, in the no-host custom view. The agents strip is gone: agents are rows in the Sessions list.
- **Agent status** maps to upstream's `SessionStatus`, and every state keeps a text label:

| wisp state | `status` | Mark in the list | Label |
| --- | --- | --- | --- |
| Queued | `InProgress`, description "Queued" | Hollow dot | Queued: after {agent} |
| Running | `InProgress` | Filled dot with a halo, `chat.sessionStateIndicator.inProgressBorder` | {current step} |
| Needs review | `NeedsInput` | Filled dot, `agentsBadge.background` | Needs review, with the diff stat |
| Conflicts with {agent} (M4, #70) | `NeedsInput` | Filled dot | Conflicts with {agent} |
| Done | `Completed` | Filled dot, `charts.green` | Done |
| Failed | `Error` | Diamond, `errorForeground` | Failed: {reason} |
| Stopped | `Completed`, with `completedStateIcon` | Square | Stopped |

### M0 (#12)

- The Agents window opens with no sign-in modal and none of Copilot's parts (#103).
- The Sessions list is empty and says why: **No projects yet. Projects live on a host, so they show here once wisp connects to one.**
- The session surface shows the no-host custom view, with the copy from coordinator-chat.md: **No host connected**, the body text, and a disabled composer whose placeholder is **Not connected to a host**. The reusable pattern from #12's branch applies: a `readonly` textarea with `aria-disabled` and `aria-describedby`, and upstream's `Button`.
- The title bar item reads **Not connected**, with a hollow dot, and does nothing yet.

## Theming

- Colors and sizes come only from theme tokens. The mocks read every value from the running Agents window, including its own tokens (`agents*`, `activeSessionView.*`, `agentsChatInput.*`, `chat.sessionStateIndicator.*`).
- `chat.*` tokens are now fine to use: upstream's chat renders wisp's sessions, so they are always registered in this window. That reverses coordinator-chat.md's rule.
- State is never shown by color alone: each mark has a distinct shape and a label.

## Keyboard and accessibility

Upstream's Agents window owns navigation: ⌘N for a new session (wisp: a new project), ⇧⌘X for the Sessions list, ⇧⌘E for files, and upstream's chat keys in the composer. wisp adds:

- the host item: a button labeled "Host: {host}, {state}", whose changes are announced politely;
- the no-host custom view: `role="region"`, labeled "No host connected", with the composer described by the reason;
- the subagent bar: its buttons are in the tab order after the transcript, where a composer would be.

Accessible View and help for transcripts come from upstream's chat widget.

## Relation to Cursor's Projects

Cursor's Agents Window puts projects and agents in a left nav, the agent chat in the center, and files, a terminal, and review on the right. Upstream's Agents window has the same shape, so wisp matches Cursor more closely than layout A did. What differs:

- Subagents are separate sessions in the list, nested under the coordinator, rather than tabs of one chat (a question for Ryan on #100).
- A subagent has no composer: you talk to the coordinator.
- The host is in the title bar, since wisp's host is a machine you own, not Cursor's cloud computer.

## Revising the mockups

The sources are in [`agents-window/src/`](agents-window/src/): `index.html` loads `tokens.css` and `agents.css`, and `mock.js` builds each scene from URL parameters (`?scene=coordinator&theme=dark-modern&notes=0`).

Playwright is not a repo dependency, so run both scripts from a temp directory that has it:

```sh
cd "$(mktemp -d)" && npm install playwright-core@1.63.0 && npx playwright-core install chromium
node <repo>/docs/design/agents-window/src/render.mjs            # writes the PNGs at 1440x900
WISP_EDITOR_DIR=<repo>/editor/vscode \
  node <repo>/docs/design/agents-window/src/capture-tokens.mjs  # after an upstream upgrade
```

`capture-tokens.mjs` needs a development build in which the Agents window opens, which means after #103 or with patch 0010 undone locally. It uses a throwaway `HOME` and profile and in-memory secret storage, so it never reads the Keychain or your own agent sessions.
