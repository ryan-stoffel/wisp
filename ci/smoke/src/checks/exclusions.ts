// #94: assert at runtime that each id wisp excludes (editor/overlay/.../wisp/exclusions.ts and
// .../excludedActions.ts) is really skipped, not just present in the list. readExclusionLists() parses the
// committed source directly, so a renamed or mis-quoted id shows up here even if check-fork's own sed missed
// it (#94's finding 1).
//
// Every one of the 58 ids gets one of four kinds of evidence, recorded below so the mapping itself is a
// checked invariant: a new exclusion has to be triaged into this table or the suite fails on the count
// mismatch, instead of silently getting no coverage.
//
//   dom      - it would render as an element whose own `id` attribute is the excluded id; assert absence.
//   process  - it starts a utility process (the local Copilot agent host); assert app.getAppMetrics() has none.
//   palette  - it (or its open command) would show as a Command Palette entry with recognizable text; assert
//              that text does not appear. Search terms are picked to avoid #125's already-tracked, unrelated
//              Copilot residue in the Agents window (upstream "Voice Input Mode" and "Chat: Open Chat (Agent)"
//              commands, neither of which shares wording with the ids checked here).
//   static   - no independent, externally observable effect: a background sync, a context key with no menu
//              entry, or a contribution reachable only through another excluded surface (for example, the
//              onboarding tours only ever launch from Automations, which is itself excluded and checked).
//              check-fork's static guard (every id still exists upstream) is this bucket's only automated
//              check today; #144 tracks giving it a real runtime signal.
import { check } from '../check.ts';
import { readExclusionLists } from '../exclusions.ts';
import { gitWorkspace, launchSmoke, ready, type SmokeSession } from '../harness.ts';
import { paletteCommands, presentElementIds, statusBarItems } from '../ui.ts';

type Method = 'dom' | 'process' | 'palette' | 'static';

interface Entry {
  readonly id: string;
  readonly method: Method;
  readonly reason: string;
}

const entries: readonly Entry[] = [
  // View containers: rendered with their own id as a DOM id when not excluded.
  entry('workbench.panel.chat', 'dom', 'the secondary side bar renders a container with its own id'),
  entry('workbench.view.debug', 'dom', 'the activity bar renders a container with its own id'),
  entry('workbench.panel.repl', 'dom', 'the panel renders a container with its own id'),
  entry('~remote.forwardedPortsContainer', 'dom', 'the panel renders a container with its own id'),
  entry('agentic.workbench.view.sessionsContainer', 'dom', "the Agents window's sidebar renders a container with its own id"),

  // Actions.
  entry('sessions.action.titleBarAccountWidget', 'static', "its contribution (sessionsWidget, below) draws the button this action opens; checking the button covers both"),
  entry('workbench.action.chat.profileAgentHost', 'process', 'it opens a view onto the same local agent host the process check covers'),
  entry('agentsVoice.simulateConnection', 'palette', 'a "Developer:" command named after the id'),
  entry('agentsVoice.resetOnboarding', 'palette', 'a "Developer:" command named after the id'),

  // Chat and Copilot core.
  entry('workbench.contrib.chatSetup', 'palette', 'the sign-in and setup commands it registers'),
  entry('workbench.contrib.chatStatusBarEntry', 'dom', 'a status bar item with its own id'),
  entry('workbench.contrib.remoteStatusIndicator', 'dom', "upstream's status bar item, id status.host"),
  entry('workbench.contrib.sessionsWidget', 'static', 'the title bar account button it draws; no id of its own, covered by the title-bar check in the Agents window check'),
  entry('workbench.contrib.sessionsCustomizationsToolbar', 'palette', 'the Customizations commands it registers'),
  entry('workbench.contrib.sessionsActiveHarnessSync', 'static', 'a background sync with no UI of its own'),
  entry('sessions.customizationsDebugLog', 'static', 'a debug log channel with no UI of its own'),
  entry('sessions.defaultSessionsProvider', 'static', "an internal provider registration; wisp's own provider replacing it is covered by the Agents window check"),
  entry('workbench.contrib.copilotPickerActionViewItems', 'static', 'changes how an existing picker renders; no id or command of its own'),
  entry('workbench.contrib.copilotPermissionPickerWeb', 'static', 'a web-only permission picker variant; the desktop app never loads it'),
  entry('workbench.contrib.chat.copilotConfigSlashSubmitHandler', 'static', 'a slash-command handler with no UI of its own'),
  entry('sessions.contrib.chat.copilotConfigSlashSubmitHandler', 'static', 'a slash-command handler with no UI of its own'),

  // Automations.
  entry('workbench.contrib.automationScheduler', 'palette', 'the Automations commands it registers'),
  entry('sessions.contrib.automationTools', 'palette', 'the Automations commands it registers'),
  entry('workbench.contrib.chatAutomationsEnabledContext', 'static', 'a context key with no command or view of its own'),
  entry('sessions.contrib.automationsCustomView', 'palette', 'the Automations view it would add'),

  // Local agent host: the utility process #127's review already used as a runtime signal.
  entry('workbench.contrib.agentHostPrewarm', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostContribution', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostTerminal', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostCopilotCliSettings', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostAllowSignedOutWhenUsable', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostSignedOutModelsNotification', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostSdkSetupNotification', 'process', 'starts the local agent host utility process'),
  entry('workbench.contrib.agentHostSessionListContribution', 'process', 'starts the local agent host utility process'),
  entry('workbench.chat.agentHostOpenSessionLinkOpener', 'process', 'starts the local agent host utility process'),
  entry('sessions.contrib.localAgentHostContribution', 'process', 'starts the local agent host utility process'),
  entry('sessions.contrib.localAgentHostLifecycle', 'process', 'starts the local agent host utility process'),

  // Remote agent hosts: reachable only from Agents window pickers this build has no ground truth text for yet.
  entry('sessions.contrib.remoteAgentHostContribution', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('workbench.contrib.remoteAgentHostTerminal', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.contrib.sshAgentHostContribution', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.contrib.tunnelAgentHostContribution', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.contrib.wslAgentHostContribution', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.contrib.devContainerAgentHostConnector', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.contrib.webSocketAgentHostContribution', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('workbench.contrib.cloudSandboxAgentHost', 'static', 'reachable only from a remote-host picker; #144 tracks a direct check'),
  entry('sessions.connectionDiagnostics', 'static', 'a diagnostics command reachable only from an active remote connection'),
  entry('workbench.contrib.sessionsTunnelHostTitlebar', 'static', 'a title bar item reachable only from an active tunnel connection'),

  // GitHub PR polling.
  entry('sessions.contrib.githubPullRequestPolling', 'palette', 'no direct command, but a "pull request" search would show its picker if it ran'),
  entry('workbench.contrib.agentFeedbackPRReviewSeeder', 'static', 'seeds review UI only when a PR poll (excluded above) finds one'),
  entry('workbench.contrib.agentFeedbackPRThreadResolver', 'static', 'resolves review threads only when a PR poll (excluded above) finds one'),

  // Onboarding tours: only ever launched from Automations or new-session flows this suite does not drive.
  entry('sessions.contrib.onboardingTours.agentHostReadinessContext', 'static', 'a context key with no command or view of its own'),
  entry('sessions.contrib.onboardingTours.newSessionTour', 'static', 'launched only from a new-session flow this suite does not drive'),
  entry('sessions.contrib.onboardingTours.newSessionViewTour', 'static', 'launched only from a new-session flow this suite does not drive'),
  entry('sessions.contrib.onboardingTours.newSessionViewV2Tour', 'static', 'launched only from a new-session flow this suite does not drive'),
  entry('sessions.contrib.onboardingTours.newSessionViewV3Tour', 'static', 'launched only from a new-session flow this suite does not drive'),

  // Copilot voice mode: background wiring with no command or id of its own.
  entry('sessions.voiceBridge', 'static', 'a background bridge with no command or view of its own'),
  entry('sessions.voiceActiveSession', 'static', 'a background state contribution with no command or view of its own'),
  entry('sessions.voiceListening', 'static', 'a background state contribution with no command or view of its own'),
  entry('sessions.voiceNewComposer', 'static', 'a composer wiring contribution with no command or view of its own'),
];

function entry(id: string, method: Method, reason: string): Entry {
  return { id, method, reason };
}

const domCandidates = entries.filter((e) => e.method === 'dom').map((e) => e.id);
const editorDomCandidates = domCandidates.filter((id) => id !== 'agentic.workbench.view.sessionsContainer');
const agentsDomCandidates = ['agentic.workbench.view.sessionsContainer'];
const upstreamRemoteStatusId = 'status.host';

const palettePlan: readonly { window: 'editor' | 'agents'; term: string; ids: readonly string[] }[] = [
  { window: 'editor', term: 'sign in', ids: ['workbench.contrib.chatSetup'] },
  { window: 'agents', term: 'customization', ids: ['workbench.contrib.sessionsCustomizationsToolbar'] },
  { window: 'agents', term: 'automation', ids: ['workbench.contrib.automationScheduler', 'sessions.contrib.automationTools', 'sessions.contrib.automationsCustomView'] },
  { window: 'agents', term: 'pull request', ids: ['sessions.contrib.githubPullRequestPolling'] },
  { window: 'agents', term: 'agents voice', ids: ['agentsVoice.simulateConnection', 'agentsVoice.resetOnboarding'] },
];

const processIds = entries.filter((e) => e.method === 'process').map((e) => e.id);
const agentHostNamePattern = /agent-?host|copilot/i;

export const exclusionChecks = [
  check('every excluded id is triaged into a checked bucket (dom, process, palette, or static)', async () => {
    const lists = await readExclusionLists();
    const parsedIds = [...lists.viewContainers, ...lists.workbenchContributions, ...lists.actions];
    const known = new Set(entries.map((e) => e.id));
    const missing = parsedIds.filter((id) => !known.has(id));
    const stale = [...known].filter((id) => !parsedIds.includes(id));
    if (missing.length > 0) {
      throw new Error(`exclusions.ts/excludedActions.ts lists ids with no entry in this suite's table: ${missing.join(', ')}`);
    }
    if (stale.length > 0) {
      throw new Error(`this suite's table has ids no longer in exclusions.ts/excludedActions.ts: ${stale.join(', ')}`);
    }
  }),

  check('excluded view containers never appear as their own DOM element', async () => {
    const workspace = await gitWorkspace();
    const editorSession = await launchSmoke([workspace.folder]);
    try {
      await ready(editorSession);
      const found = await presentElementIds(editorSession.window, editorDomCandidates);
      if (found.length > 0) {
        throw new Error(`found excluded view container(s) in the editor window: ${found.join(', ')}`);
      }
      const remoteIndicator = await presentElementIds(editorSession.window, [upstreamRemoteStatusId]);
      if (remoteIndicator.length > 0) {
        throw new Error(`upstream's remote indicator status bar item is present (${upstreamRemoteStatusId})`);
      }
      const statusBar = await statusBarItems(editorSession.window);
      const copilotStatus = statusBar.filter((item) => /copilot|chat\.statusBarEntry/i.test(`${item.id} ${item.text}`));
      if (copilotStatus.length > 0) {
        throw new Error(`status bar has a Copilot item: ${copilotStatus.map((s) => s.id).join(', ')}`);
      }
    } finally {
      await editorSession.close();
      await workspace.cleanup();
    }

    const agentsSession = await launchSmoke();
    try {
      await ready(agentsSession);
      const found = await presentElementIds(agentsSession.window, agentsDomCandidates);
      if (found.length > 0) {
        throw new Error(`found excluded view container(s) in the Agents window: ${found.join(', ')}`);
      }
    } finally {
      await agentsSession.close();
    }
  }),

  check('the local agent host utility process never starts', async () => {
    const workspace = await gitWorkspace();
    let agentsSession: SmokeSession | undefined;
    let editorSession: SmokeSession | undefined;
    try {
      agentsSession = await launchSmoke();
      await ready(agentsSession);
      await agentsSession.window.waitForTimeout(5_000);
      const agentsProcesses = await processNames(agentsSession);

      editorSession = await launchSmoke([workspace.folder]);
      await ready(editorSession);
      await editorSession.window.waitForTimeout(5_000);
      const editorProcesses = await processNames(editorSession);

      const flagged = [...agentsProcesses, ...editorProcesses].filter((name) => agentHostNamePattern.test(name));
      if (flagged.length > 0) {
        throw new Error(`found a process that looks like the local agent host: ${flagged.join(', ')} (covers ${String(processIds.length)} excluded ids)`);
      }
    } finally {
      await agentsSession?.close();
      await editorSession?.close();
      await workspace.cleanup();
    }
  }),

  check('excluded features have no matching command palette entry', async () => {
    const workspace = await gitWorkspace();
    let editorSession: SmokeSession | undefined;
    let agentsSession: SmokeSession | undefined;
    try {
      const hits: string[] = [];
      for (const plan of palettePlan) {
        if (plan.window === 'editor') {
          editorSession ??= await openReady(() => launchSmoke([workspace.folder]));
          const rows = await paletteCommands(editorSession.window, plan.term);
          hits.push(...rows.map((row) => `${plan.term} (editor): ${row}`));
        } else {
          agentsSession ??= await openReady(() => launchSmoke());
          const rows = await paletteCommands(agentsSession.window, plan.term);
          hits.push(...rows.map((row) => `${plan.term} (agents): ${row}`));
        }
      }
      if (hits.length > 0) {
        throw new Error(`command palette has entries an excluded feature would show: ${hits.join(' | ')}`);
      }
    } finally {
      await editorSession?.close();
      await agentsSession?.close();
      await workspace.cleanup();
    }
  }),
];

async function openReady(open: () => Promise<SmokeSession>): Promise<SmokeSession> {
  const session = await open();
  await ready(session);
  return session;
}

interface ProcessMetricLike {
  readonly type: string;
  readonly serviceName?: string;
  readonly name?: string;
}

interface AppMetricsHandle {
  readonly app: { getAppMetrics(): ProcessMetricLike[] };
}

async function processNames(session: SmokeSession): Promise<string[]> {
  return session.app.evaluate((electron: AppMetricsHandle) =>
    electron.app
      .getAppMetrics()
      .map((metric) => `${metric.type}${metric.serviceName ? `:${metric.serviceName}` : ''}${metric.name ? `:${metric.name}` : ''}`),
  );
}
