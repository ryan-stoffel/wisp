# 0011: wisp builds on upstream's Agents window, laid out like Cursor's Projects

- Status: accepted
- Date: 2026-09-24
- Issue: #100

## Context

Ryan decided on #14: "The ui for v1 should almost reuse the agents window from vscode, that is a much closer BASE line that we can build off of." That replaces layout A from #11, a coordinator view in the editor window's secondary side bar. #10 had stripped the editor for layout A: it keeps Chat's container out of registration and sends every request for the Agents window to a regular window.

The reference product is Cursor's Projects, and Ryan's screenshot of it is now the reference layout (#100): a coordinator that plans and delegates, and subagents that work in parallel in their own worktrees. wisp's agents are vendor CLIs (Claude Code, Codex) that `wispd` runs on a host (0004, 0007), not Copilot.

Everything below was read in Code - OSS 1.139.0, prepared with `scripts/editor/prepare`. Paths are relative to `editor/vscode/src/vs/` unless they start with `editor/` or `docs/`.

## What the Agents window is

**A second workbench, not a part of the editor window.** `sessions/` is a layer above `workbench/` with 930 files and its own specifications (`sessions/README.md`, `SESSIONS.md`, `LAYOUT.md`, `LAYERS.md`, `SESSIONS_LIST.md`). `workbench/` must not import from it.

- **Window.** A separate `BrowserWindow` that loads `sessions/electron-browser/sessions.html` (`platform/windows/electron-main/windowImpl.ts`, line 1296) when its workspace is the agent sessions workspace (`environmentService.agentSessionsWorkspace`). `windowsMainService.ts` marks it `isSessionsWindow` and gives it its own profile, the Agents window profile (`createAgentsWindowProfile`).
- **Entry points.** They all go through `WindowsMainService.openAgentsWindow` and `ensureAgentsWindow`:
  - the `--agents` flag (`platform/environment/node/argv.ts`, line 111; `code/electron-main/app.ts`, `openFirstWindow`);
  - the command `workbench.action.openAgentsWindow` (`workbench/contrib/chat/common/constants.ts`, line 609; `platform/native/electron-main/nativeHostMainService.ts`, `openAgentsWindow`);
  - agent session protocol links (`app.ts`, `parseExternalOpenSessionLinkUri`);
  - a second instance started with `--agents`.
- **Code.** `sessions/sessions.desktop.main.ts` imports `sessions.common.main.ts`, which imports most of the workbench's contributions and then the Agents window's own. It does not import `workbench/workbench.desktop.main.ts`, so wisp's editor-window contribution (patch 0008) never runs there.
- **Parts** (`sessions/LAYOUT.md`). A title bar; a sidebar with the Sessions list (`sessions/contrib/sessions`) and, below it, AI Customizations; the Sessions part, which shows one or more sessions' chats; an editor and a detail pane; an auxiliary bar with Changes and Files (`sessions/contrib/changes`, `sessions/contrib/files`); a panel with the terminal; and a Custom View Grid for full-surface contributed views (`sessions/services/customView`). **It has no activity bar and no status bar.** At 1440x900 it lays out a 35 px title bar, a 280 px sidebar, the Sessions part, and a 300 px auxiliary bar.

## What it depends on

| Dependency | Where | Needed by wisp |
| --- | --- | --- |
| Chat: the chat widget, chat model, `IChatSessionsService` | `workbench/contrib/chat/**`, imported by `sessions.common.main.ts` (`chat.shared.contribution.js`, `chatSessions.contribution.js`) | Yes. Every session's transcript is a chat. |
| AI features on | `sessions/browser/sessionsSetUpService.ts` turns `chat.disableAIFeatures` off at the Agents window's workspace scope (`sessions/services/configuration/browser/configurationService.ts`, line 174), or blocks with an "Enable AI Features" dialog | Yes, in that window only |
| Sessions services and providers | `sessions/services/sessions/**`: `ISessionsProvidersService`, `ISessionsManagementService`, `ISessionsService` | Yes |
| GitHub or Copilot sign-in | `sessionsSetUpService.ts` and `sessionsAuthGate.ts` show "Sign in to use Agents" (GitHub, Google, Apple, GHE) unless `product.defaultChatAgent.chatExtensionId` is empty, or a session type can initialize without GitHub | No |
| The agent host | `platform/agentHost/**`, 2,569 files: a utility process that runs Copilot CLI, Claude, and Codex harnesses behind the Agent Host Protocol. The window starts it on open. | No |
| Language models | The model picker reads `ILanguageModelsService` through the provider's `getModelsSnapshot` | Only through wisp's own provider (M2 accounts) |

Calls to Microsoft or GitHub services that the window's code can make:

- GitHub sign-in, and Copilot's API at `api.githubcopilot.com`. The agent host's Claude harness sends Anthropic traffic through a local proxy to Copilot's API by default (`platform/agentHost/node/claude/CONTEXT.md`, `claudeTransportMode.ts`). Its "native" mode, on the user's own credentials, is behind the experimental `chat.agentHost.allowSignedOutWhenUsable`.
- `main.vscode-cdn.net`, for the Claude and Codex SDKs (`build/agent-sdk/common.ts`, `platform/agentHost/node/agentSdkDownloader.ts`). Code - OSS ships no `product.agentSdks`, so in wisp those two harnesses are unavailable unless a developer override is set.
- `api.github.com`, for pull request and issue polling (`sessions/contrib/github`).
- Microsoft dev tunnels and GitHub cloud sandboxes, for remote agent hosts (`sessions/contrib/providers/remoteAgentHost`).

Measured: the #100 experiment ran the window through a blocking proxy for about 16 seconds per launch, with the Copilot extension disabled. The only outbound attempt was `raw.githubusercontent.com`, #9's Open VSX extension-control list. The proxy sees Chromium traffic and processes that honor `HTTP(S)_PROXY`; it is not a proof for processes that ignore both.

## Without Copilot

Screenshots are in `docs/design/agents-window/upstream/`.

- **It opens.** With the redirect undone (patch 0010), `--agents` opens the window whether #10's exclusions are in place or not.
- **With upstream's defaults**, a first launch shows the "Sign in to use Agents" modal (`1-sign-in-gate.png`). With no sign-in extensions in wisp's app, it can't be completed.
- **Past the modal** (`--skip-sessions-welcome`), the window is an empty shell (`2-`, `3-`, `4-`). It has no session types and no models, **Send** does nothing, and the workspace picker offers Local, GitHub, and Remote.
- **With #10's exclusions and `chat.disableAIFeatures` on**, it opens with no modal, no title bar actions, and an empty Customizations section (`5-with-10s-exclusions.png`). Nothing works either.

The minimum to keep is the `sessions/` layer, the chat stack it imports, and the sessions services. Everything Copilot-shaped can be excluded from registration (0002 rule 4): the account menu, AI Customizations, Automations, the Copilot Chat sessions provider, the local agent host and its process, remote agent hosts, the GitHub integration, onboarding tours, and voice. #103 does that.

## The provider API

**Upstream has one, and it runs in process.** `ISessionsProvider` (`sessions/services/sessions/common/sessionsProvider.ts`) is the contract every backend implements: Copilot Chat (`sessions/contrib/providers/copilotChatSessions`), the local agent host, and remote agent hosts. A provider registers with `ISessionsProvidersService.registerProvider` from a contribution in the Agents window's renderer. It publishes session types, a session catalog, workspaces, and capabilities, and it handles drafts, sends, archive, delete, and chats.

- A session (`sessions/services/sessions/common/session.ts`) exposes observables for `status` (`InProgress`, `NeedsInput`, `Completed`, `Error`), `title`, `description`, `workspace`, `changes`, `changesets`, `artifacts`, `remoteConnectionStatus`, `createdBySession`, and `chats`.
- A chat (`IChat`, same file) has its own `status`, `title`, `changes`, `description`, `origin` (`ChatOriginKind.Tool` with a `parentChat` for a subagent), and `interactivity` (`Full`, `ReadOnly`, or `Hidden`). The file's own comment names "a worker (subagent) chat" as a use.
- The transcript comes from `IChatSessionsService.registerChatSessionContribution` and `registerChatSessionContentProvider` (`workbench/contrib/chat/common/chatSessionsService.ts`, lines 828 and 884). An `IChatSession` supplies history, a progress observable, and a request handler.
- The extension API for chat sessions (`workbench/api/browser/mainThreadChatSessions.ts`) calls those same services. A built-in contribution calls them directly, so **no extension host round trip is involved**. `workbench/contrib/chat/browser/agentSessions/agentHost/agentHostChatContribution.ts` is the in-tree example: it registers the agent host's sessions this way.

**An extension cannot do it.** Chat session types contributed by extensions reach the editor window's agent sessions view, but the Agents window's Copilot provider keeps only Copilot's own types (`copilotChatSessionsProvider.ts`, `_refreshSessionCache`: `Background` and `Cloud`). So wisp's sessions must come from a provider in the `sessions/` layer.

## Ryan's decisions (#100, 2026-09-24)

1. **Launch:** Wisp opens straight into the Agents window.
2. **Sidebar:** it follows his Cursor Projects screenshot. From the top: actions (New Chat, Search, Automations, Customize); **Projects**, one row per project, where a project is a single thread, its coordinator; **Repositories**, normal threads grouped under each repo; **No Repo**; and a footer with the user and settings. Threads the coordinator spawns never appear in the sidebar.
3. **Subagents:** an **Agents** pill above the coordinator's composer opens an Agents panel over the transcript, with a close button. Each row shows a status dot, the task title, and where the agent runs (a monitor for this Mac, the host's name for the host). The panel shows about five rows, then **More**. Clicking a row opens that subagent as a tab next to the coordinator in the center tab strip, with an enabled composer, so you can message it directly. The reference is `docs/design/agents-window/reference/cursor-agents-pill.png`.
4. **Naming:** Wisp's own labels replace upstream's, at the cost of a few patches.

## Decision

1. **The Agents window is wisp's main UI, and it opens at launch.** The editor window stays for quick edits, reached through the **IDE** button. A `startup:` patch in `code/electron-main/app.ts` opens the Agents window when wisp starts with no file or folder arguments, and when the macOS dock reopens the app with no windows. `wisp <folder>` and `wisp <file>` still open the editor window (#103).
2. **wisp implements upstream's provider contract in built-in code.** `WispSessionsProvider` lives in the overlay at `src/vs/sessions/contrib/providers/wisp/`, which upstream's layer rules allow (`sessions/LAYERS.md`). It registers in process and gets its data from the editor's wispd connection (0007). It is not an extension.
3. **wisp owns the left sidebar view.** Upstream's Sessions list (`sessions/contrib/sessions/browser/views/sessionsList.ts`) has no Projects or Repositories sections, and it is upstream's busiest file: 932 lines changed from 1.138.0 to 1.139.0 alone. Patching it into Ryan's layout would conflict on nearly every upgrade. Instead:
   - wisp excludes upstream's Sessions container, `agentic.workbench.view.sessionsContainer`, through the exclusion list;
   - wisp registers its own default sidebar container, `wisp.threads`, with `windowEnablement: WindowEnablement.Sessions`;
   - its view renders the actions, Projects, Repositories, No Repo, and the footer;
   - it reads sessions through `ISessionsManagementService` and opens them through `ISessionsService`. Those are provider-neutral and slower-moving: `sessionsManagement.ts` changed 11 lines from 1.138.0 to 1.139.0.
   The labels there are wisp's own strings, so they cost no patches.
4. **Sessions and chats map as upstream intends.** A project is one session whose main chat is the coordinator. A subagent is a chat in that session with `origin.kind = ChatOriginKind.Tool`, `origin.parentChat` set to the coordinator's chat, and `interactivity: Full`. Upstream already:
   - keeps tool-origin chats out of sidebar rows (`sessionsList.ts`, line 217), which wisp's own view copies;
   - lists a chat's direct subagents in a pill above its composer (`sessions/contrib/chat/browser/sessionBackgroundActivitiesControl.ts`);
   - opens a subagent with `ISessionsService.openChat`, which shows it as a tab next to the main chat. The strip appears once more than one chat is visible (`sessions/services/sessions/browser/visibleSessions.ts`, `shouldShowChatTabs`), and closing the tab hides the subagent again.
   So Ryan's tab behavior is upstream's, with no patch.
5. **A normal thread is its own session** of type `wisp.thread`: one agent (Claude Code or Codex, per 0004) that wispd runs on the host in a repo, or in a scratch folder for No Repo. It has no coordinator (#110).
6. **One patch loads wisp in the Agents window.** A line in `sessions/sessions.desktop.main.ts` imports `src/vs/sessions/contrib/wisp/browser/wisp.sessions.contribution.ts` from the overlay, as patch 0008 does in the editor window. Settings with `agentsWindow: { default, readOnly }` overrides win over `registerDefaultConfigurations` there (#12's findings).
7. **wispd keeps its own protocol.** wisp does not speak upstream's Agent Host Protocol (see Rejected). Messaging a subagent directly needs one additive method under the `agents` capability, `agent/send {runId, turnId, text}`. wispd also reports the message to the coordinator, so its plan stays accurate.
8. **Copilot's parts of the window are excluded from registration**, with the same list and guard as #10 (#103).

### How wisp maps onto it

| wisp | Agents window | How | Milestone |
| --- | --- | --- | --- |
| Actions: New Chat, Search, Automations, Customize | Top of wisp's sidebar view | New Chat opens upstream's new-session composer for `wisp.thread`. Search is a Quick Pick over projects and threads. Automations is upstream's Automations surface once #18 decides triggers. Customize opens wisp's settings: hosts, then accounts. | M1; Automations M6; Customize M2 |
| Project | A row under Projects: glyph, name, relative age. `+` starts a new project. | A `wisp.project` session. Its main chat is the coordinator. | M1 shell, M4 live (#104) |
| Normal thread | A row under its repo in Repositories, or under No Repo | A `wisp.thread` session with a workspace, or a quick chat for No Repo | M3 (#110) |
| Subagent | Hidden from the sidebar. Listed in the Agents panel from the pill, and opened as a tab | A tool-origin chat of the project session, `interactivity: Full` | M3 (#105) |
| Agent status | The dot in the Agents panel and on the tab, plus the row's state text | `IChat.status` plus `description` | M3 (#105) |
| Where an agent runs | A monitor and "this Mac", or a server glyph and the host's name, in the panel and in the composer footer | The chat's `description`, rendered as the pill entry's `badge` | M3, M5 for local agents |
| Worked summaries, replies, feedback | Upstream's chat widget: collapsed "Worked for Xs" groups (`chatSubagentOpenChat.ts`, line 517), thumbs, and copy | wisp's `IChatSession` progress and tool invocations | M4 |
| Action cards and chips above the composer | For example "Render invoice PDFs is ready for review" with Review and Accept, and suggestion chips | Chat confirmation parts and follow-ups from wisp's content provider | M4 |
| Composer footer | Branch and host, for example `main` and `mac-mini` | A contribution to the session chat input toolbar | M1 |
| IDE button | The center card's toolbar | Upstream's **Open in Editor** action, relabeled | M0 (#103) |
| Right panel | Tabs: **Project**, **Changes**, **Files** | A `wisp.project` container in the auxiliary bar, first and default, holding project facts and shared context. Changes and Files are upstream's. | M1 facts, M3 shared context (#104, #106) |
| Diff review per worktree | Changes tab for the viewed chat's worktree; the review editor in the detail pane | `IChat.changes`, and `ISession.changesets` with one changeset per agent worktree | M3 (#105) |
| Host status | A chip in the sidebar footer, opening the host Quick Pick. The composer footer names the host for each thread. | wisp's sidebar view; `remoteConnectionStatus` on sessions | M0 static, M1 live (#12, #65, #104) |
| No host connected | A Custom View Grid view over the session surface | `AbstractCustomView` (`sessions/services/customView/browser/customView.ts`) | M0 (#12) |

### The Agents panel

Upstream's pill is close to Ryan's reference, but not the same:

- **Label.** Upstream titles it "Subagents"; wisp wants "Agents". That is a string patch.
- **Rows.** Upstream draws one agent icon per entry. wisp wants the chat's status as the icon and its location as the trailing `badge`; `IChatPillEntry` already has both fields. That is about 10 lines in `sessionBackgroundActivitiesControl.ts`, a 90-line file that didn't change from 1.138.0 to 1.139.0.
- **Presentation.** Upstream opens the entries as a dropdown anchored to the pill (`workbench/browser/chatPills.ts`). Ryan's reference is a card over the transcript with a close button and **More** after about five rows. #105 first tries to get there through the pill's existing presentation options. If it can't, it takes a patch in `chatPills.ts`: estimated 40 to 60 lines, in a file that changed 27 lines from 1.138.0 to 1.139.0.

### Rejected

| Option | Why not |
| --- | --- |
| Patch upstream's Sessions list into Ryan's layout | Its file changed 932 lines in one week, so structural patches there would conflict on almost every upgrade. |
| Subagents as sessions in the sidebar | Ryan: the coordinator's threads are not listed there. |
| Subagents as always-visible tabs, plus a list in the right panel (the coordinator's first proposal) | Replaced by Ryan's Agents pill decision |
| Opening a subagent in place of the coordinator, with a back button, or stacked under it | Ryan chose tabs, which are upstream's behavior and cost no patch. |
| wispd speaks the Agent Host Protocol, and upstream's remote agent host provider connects to it over SSH | It would put upstream's fastest-moving protocol on wispd's side of 0007. The protocol has no version contract for other servers, and the agent host that defines it changes every week. Hosts and editors would have to upgrade together. |
| Run Claude and Codex in upstream's local agent host | It runs agents on the laptop and bypasses wispd, which breaks M4's "keeps working with the MacBook closed". By default it sends Claude through Copilot's API. It needs SDKs from Microsoft's CDN, and it is not the unmodified vendor CLI that 0004 requires. |
| A wisp extension that contributes chat session types | The Agents window drops extension-contributed session types, and it would add an extension host round trip to every event. |

## What changes in 0002

- **Rule 1** (configuration, then new files, then small edits) holds. The provider, the sidebar view, and the Project panel are overlay files.
- **Rule 5 changes.** It said to keep wisp's features out of upstream's chat, sessions, and agent host code. Now:
  - wisp implements upstream's contracts (`ISessionsProvider`, `IChatSessionsService`, `AbstractCustomView`, view containers) from its own files;
  - it edits upstream's sessions and chat code only with one-line imports, exclusions, string changes, and the small pill change above, each its own patch;
  - it never patches `sessionsList.ts`;
  - upstream's chat widget renders wisp's transcripts, so the old "no `chat.*` theme tokens" rule no longer applies in the Agents window.

### Patch surface

New patches, on top of #10's six:

| Patch | File | Size | Upstream churn, 1.138.0 to 1.139.0 |
| --- | --- | --- | --- |
| `startup: open the Agents window at launch` | `code/electron-main/app.ts` | 2 hunks, about 8 lines | 21 lines |
| `workbench: load wisp in the Agents window` | `sessions/sessions.desktop.main.ts` | 1 line | 0 lines (8 over four releases) |
| `branding: call Open in Editor IDE` | `sessions/{browser,electron-browser}/actions/vscodeActions.ts` | 2 lines | 0 lines |
| `branding: call subagents Agents in the pill` | `sessionBackgroundActivitiesControl.ts` | 1 line | 0 lines |
| `chat: show status and location in the Agents pill` | Same file | about 10 lines | 0 lines |
| `chat: present the Agents pill as a panel`, only if needed | `workbench/browser/chatPills.ts` | 40 to 60 lines | 27 lines |
| Reverted: patch 0010, the Agents window redirect | | -1 line | |

That is four to six patches and about 20 to 80 changed lines. The rest is overlay code, which breaks at type-check rather than at rebase when upstream changes the interfaces it implements. From 1.138.0 to 1.139.0, `sessionsProvider.ts` changed 11 lines, `session.ts` 30, and `sessionsManagement.ts` 11. Over four releases they changed 81, 221, and 31. Budget an extra 30 to 45 minutes per monthly upgrade (0002's estimate): about 10 for the patches, which rarely conflict, and the rest to adapt the provider and sidebar view to interface changes. Keep the provider thin: it maps wispd's events to upstream's facades and holds no logic of its own.

## What changes in #10 and #93

**#10**, as a concrete issue, #103:

- Remove patch 0010, the Agents window redirect, and add the startup patch.
- Add the Agents window's import patch and entry point.
- Extend the exclusion list to the Agents window's Copilot and Microsoft-service parts, including upstream's Sessions container, which wisp's own sidebar replaces. Keep the local agent host from starting.
- Turn off the sign-in gate.
- Rename **Open in Editor** to **IDE**.
- Keep the rest: Chat's container, chat setup, and the Copilot status item stay excluded in the editor window; `chat.disableAIFeatures` stays `true` there; and the build still leaves the Copilot and sign-in extensions out.

**#93** asked to make `chat.disableAIFeatures` application-scoped. That would break the Agents window, which writes the setting at its own workspace scope and blocks with a dialog if it can't turn it off. The criterion changes: keep AI features off in the editor window only. The type acquisition criteria are unchanged.

## Consequences

- wisp gets Cursor's Projects layout from upstream parts: the session surface, chat tabs, the subagent pill, the Changes view, and the review editor. It owns only the sidebar and the Project panel.
- wisp inherits upstream's churn in the interfaces it implements. `typecheck-client` catches it, and upgrades take longer.
- `docs/design/coordinator-chat.md` is superseded by `docs/design/agents-window.md`. Its states, copy, and error table carry over.
- #12's M0 scope is the launch into the Agents window with wisp's sidebar, an empty provider, and the no-host view. #103 does the stripping and patches. #104, #105, #106, and #110 cover M1 and M3.
- wispd's protocol gains `agent/send` under the `agents` capability (#105).
