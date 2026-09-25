# 0011: wisp builds on upstream's Agents window through a built-in sessions provider

- Status: accepted
- Date: 2026-09-24
- Issue: #100

## Context

Ryan decided on #14: "The ui for v1 should almost reuse the agents window from vscode, that is a much closer BASE line that we can build off of." That replaces layout A from #11, a coordinator view in the editor window's secondary side bar. #10 had stripped the editor for layout A: it keeps Chat's container out of registration and sends every request for the Agents window to a regular window.

The reference product is Cursor's Projects: a coordinator that plans and delegates, and subagents that work in parallel in their own worktrees. wisp's agents are vendor CLIs (Claude Code, Codex) that `wispd` runs on a host (0004, 0007), not Copilot.

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
- The transcript comes from `IChatSessionsService.registerChatSessionContribution` and `registerChatSessionContentProvider` (`workbench/contrib/chat/common/chatSessionsService.ts`, lines 828 and 884). An `IChatSession` supplies history, a progress observable, and a request handler.
- The extension API for chat sessions (`workbench/api/browser/mainThreadChatSessions.ts`) calls those same services. A built-in contribution calls them directly, so **no extension host round trip is involved**. `workbench/contrib/chat/browser/agentSessions/agentHost/agentHostChatContribution.ts` is the in-tree example: it registers the agent host's sessions this way.

**An extension cannot do it.** Chat session types contributed by extensions reach the editor window's agent sessions view, but the Agents window's Copilot provider keeps only Copilot's own types (`copilotChatSessionsProvider.ts`, `_refreshSessionCache`: `Background` and `Cloud`). So wisp's sessions must come from a provider in the `sessions/` layer.

## Decision

1. **wisp's Agents window is its main UI.** The editor window stays for quick edits, reached through upstream's **Open in Editor**. Whether wisp opens in the Agents window at launch is a question for Ryan (below). Until he answers, the default is upstream's: the editor window at launch, and `wisp --agents` for the Agents window.
2. **wisp implements upstream's provider contract in built-in code.** `WispSessionsProvider` lives in the overlay at `src/vs/sessions/contrib/providers/wisp/`, which upstream's layer rules allow (`sessions/LAYERS.md`: providers may import contributions and services). It registers in process and gets its data from the editor's wispd connection (0007). It is not an extension.
3. **One patch loads it.** A line in `sessions/sessions.desktop.main.ts` imports a wisp entry point from the overlay, `src/vs/sessions/contrib/wisp/browser/wisp.sessions.contribution.ts`, as patch 0008 does in the editor window. Views and containers wisp adds to that window set `windowEnablement: WindowEnablement.Sessions`: `getDefaultViewContainer` filters on it, and the default is the editor window only. Several settings carry `agentsWindow: { default, readOnly }` overrides, which win over `registerDefaultConfigurations` there (#12's findings).
4. **wispd keeps its own protocol.** wisp does not speak upstream's Agent Host Protocol (below).
5. **Copilot's parts of the window are excluded from registration**, with the same list and guard as #10 (#103).

### How wisp maps onto it

| wisp | Agents window | Milestone |
| --- | --- | --- |
| Project | A workspace section in the Sessions list (`ISessionWorkspace`), labeled with its host. **New project** is a provider browse action. | M1 (#104) |
| Coordinator chat | One session per project, of type `wisp.coordinator`. Its main chat is the coordinator transcript, and a send is `coordinator/send`. | M1 shell, M4 live (#104) |
| Subagent | Its own session, of type `wisp.agent`, with `createdBySession` set to the coordinator, so the list places it right under its creator (`SESSIONS_LIST.md`). Read-only transcript from `agent/output`. | M3 (#105) |
| Agent status | `status`, plus `description` for the current step. Running is `InProgress`; needs review is `NeedsInput`; done, stopped, and failed are `Completed` or `Error`, with `completedStateIcon`; queued is `InProgress` with the description "Queued". Every state has a text label. | M3 (#105) |
| Diff review per worktree | `changes` and `changesets` from `agent/diff`, one worktree per agent. Upstream's Changes view in the auxiliary bar and its Changes editor show them. Accept and Request changes are menu contributions on the changeset (#68). | M3 (#105) |
| Host status | A title bar item, because the window has no status bar. Sessions also report `remoteConnectionStatus`. | M0 static, M1 live (#12, #65, #104) |
| Shared context | A `wisp.sharedContext` view in the sidebar, in the slot of upstream's AI Customizations, backed by a `wisp-context:` file system provider (0005) | M3 (#106) |
| Accounts | The composer's model picker, fed by the provider's `getModelsSnapshot` | M2 |
| No host connected | A Custom View Grid view (`AbstractCustomView`, `sessions/services/customView/browser/customView.ts`), which replaces the session surface until wisp connects | M0 (#12) |

### Rejected

| Option | Why not |
| --- | --- |
| wispd speaks the Agent Host Protocol, and upstream's remote agent host provider connects to it over SSH | It would put upstream's fastest-moving protocol on wispd's side of 0007. The protocol is internal: it has no version contract for other servers, and the agent host that defines it is 2,569 files that change every week. wispd would need changes for every editor upgrade, and hosts and editors would have to upgrade together. |
| Run Claude and Codex in upstream's local agent host | That runs agents on the laptop, not the host, and bypasses wispd, so it breaks M4's "keeps working with the MacBook closed". By default it sends Claude through Copilot's API. Its native mode is experimental and needs SDKs from Microsoft's CDN, which Code - OSS doesn't configure. It is not the unmodified vendor CLI that 0004 requires. |
| A wisp extension that contributes chat session types | The Agents window drops extension-contributed session types (above), and it would add an extension host round trip to every event. |
| Keep layout A in the editor window | Ryan's decision on #14 |

## What changes in 0002

- **Rule 1** (configuration, then new files, then small edits) holds. The provider and its views are new overlay files, and the cost is one import patch.
- **Rule 5 changes.** It said to keep wisp's features out of upstream's chat, sessions, and agent host code, and that the coordinator should be its own contribution, not an edit to upstream's chat. Now:
  - wisp still never edits upstream's chat, sessions, or agent host files. The only patches there are one-line imports and exclusions, and exclusions go through the existing registry filter.
  - wisp implements upstream's contracts (`ISessionsProvider`, `IChatSessionsService`, `AbstractCustomView`) from its own files. Upstream's chat widget renders wisp's transcripts, so the old "no `chat.*` theme tokens" rule no longer applies inside the Agents window.
- **The cost moves from patches to type-checks.** wisp's overlay code compiles against interfaces that change often. From 1.135.0 to 1.139.0, four releases, `sessionsProvider.ts` changed 81 lines and `session.ts` 221; from 1.138.0 to 1.139.0, 11 and 30. `chatSessionsService.ts` changed 19 lines over the four releases, and `sessions.desktop.main.ts`, where the import patch goes, 8. `check-fork`'s `typecheck-client` already fails an upgrade that breaks the provider. Budget an extra 15 to 30 minutes per monthly upgrade (0002's estimate) to adapt it, and keep the provider thin: it maps wispd's events to upstream's facades and holds no logic of its own.

## What changes in #10 and #93

**#10**, as a concrete issue, #103:

- Remove patch 0010, the Agents window redirect.
- Add the Agents window's import patch and entry point.
- Extend the exclusion list to the Agents window's Copilot and Microsoft-service parts, and keep the local agent host from starting.
- Turn off the sign-in gate: an empty `defaultChatAgent.chatExtensionId`, or `canInitializeWithoutGitHub` from wisp's provider.
- Keep the rest: Chat's container, chat setup, and the Copilot status item stay excluded in the editor window; `chat.disableAIFeatures` stays `true` there; and the build still leaves the Copilot and sign-in extensions out.

**#93** asked to make `chat.disableAIFeatures` application-scoped, so a workspace can't turn it off. That would break the Agents window, which writes the setting at its own workspace scope and blocks with a dialog if it can't turn it off. The criterion changes: keep AI features off in the editor window by refusing workspace values of the setting there, or by keeping everything it would enable excluded (it already brings back no Chat UI, per #10's Handoff). It must stay settable in the Agents window's workspace. The type acquisition criteria are unchanged.

## Questions for Ryan

Posted on #100 with mockups:

1. Should wisp open in the Agents window at launch, with `wisp <folder>` opening the editor window? That takes a small patch in `code/electron-main/app.ts`.
2. Subagents as separate sessions nested under the coordinator (proposed), or as chats inside the coordinator's session?
3. Can you message a subagent directly? Proposed: no. Agents answer to the coordinator, and a subagent's session offers **Ask the coordinator** instead of a composer.
4. Keep upstream's names ("Sessions", "Open in Editor"), which costs nothing, or rename them ("Projects"), which takes patches in upstream's strings?

## Consequences

- wisp gets a Cursor-like projects UI without writing the list, the session surface, the diff review, or the layout. Upstream keeps improving them.
- wisp inherits upstream's churn in exactly the interfaces it implements. `typecheck-client` catches it, and upgrades take longer.
- `docs/design/coordinator-chat.md` is superseded by `docs/design/agents-window.md`. Its states, copy, and error table carry over.
- #12's M0 scope is the provider shell and the no-host view in the Agents window. #104, #105, and #106 cover M1 and M3.
- A patch to upstream's startup is needed only if Ryan wants the Agents window at launch.
