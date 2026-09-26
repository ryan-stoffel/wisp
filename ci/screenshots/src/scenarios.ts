import { cp } from 'node:fs/promises';
import { join } from 'node:path';
import { agentReply, agentTab, agentTask, chatShows, fakeClaudeEnv, openAgentsPanel, sendMessage, startSubagent } from './agents.ts';
import { addSharedContextFile } from './context.ts';
import { screenshot, visible, type Scenario } from './harness.ts';
import { connectedToThisMac, createProject, hostChip, hostChipIn, projectName, projectRow, projectTab } from './projects.ts';

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = 'tasks.ts';

/**
 * An ssh destination nothing listens on, so ssh fails at once with "connection refused" (no
 * route). An unresolvable name can take a DNS timeout of 30 s or more, longer than the handshake.
 */
const unreachableHost = 'ssh://127.0.0.1:9';

export const scenarios: readonly Scenario[] = [
  {
    name: 'agents-window',
    title: 'Agents window connected to this Mac',
    async run({ window }) {
      // The packaged app's own wispd starts through wispd attach, as for a user (#62, 0010).
      await connectedToThisMac(window);
      return screenshot(window);
    },
  },
  {
    name: 'startup',
    title: 'Empty editor window',
    args: () => Promise.resolve(['--new-window']),
    async run({ window }) {
      await visible(window, '.part.titlebar', '.part.activitybar', '.part.editor', '.part.statusbar');
      return screenshot(window);
    },
  },
  {
    name: 'editor-file-open',
    title: 'Editor with a file open',
    async args(dir) {
      const folder = join(dir, 'workspace');
      await cp(workspace, folder, { recursive: true });
      return [folder, join(folder, 'src', openFile)];
    },
    async run({ window }) {
      await visible(
        window,
        `.monaco-editor[data-uri$="/src/${openFile}"] .view-lines`,
        `.tabs-container .tab.active[data-resource-name="${openFile}"]`,
        `[id="workbench.view.explorer"] .monaco-list-row[aria-label="${openFile}"]`,
      );
      return screenshot(window);
    },
  },
  {
    name: 'agents-window-disconnected',
    title: 'Agents window with a host it cannot reach',
    settings: { 'wisp.host': unreachableHost },
    async run({ window }) {
      await visible(window, '.part.titlebar', '.part.sidebar .wisp-threads');
      await hostChipIn(window, 'error', `Host: ${unreachableHost}, unreachable`);
      const view = window.locator('.wisp-agents-no-host[data-kind="error"]');
      await view.waitFor({ state: 'visible' });
      const input = view.locator('textarea');
      const state = {
        heading: await view.locator('h2').textContent(),
        placeholder: await input.getAttribute('placeholder'),
        readonly: await input.evaluate((element) => (element as HTMLTextAreaElement).readOnly),
        inputAriaDisabled: await input.getAttribute('aria-disabled'),
        sendAriaDisabled: await view.locator('.wisp-no-host-send').getAttribute('aria-disabled'),
      };
      const expected = {
        heading: `Can't reach ${unreachableHost}.`,
        placeholder: 'Reconnect to send messages',
        readonly: true,
        inputAriaDisabled: 'true',
        sendAriaDisabled: 'true',
      };
      if (JSON.stringify(state) !== JSON.stringify(expected)) {
        throw new Error(`the no-host view does not show the unreachable host: ${JSON.stringify(state)}`);
      }
      return screenshot(window);
    },
  },
  {
    name: 'agents-window-host-menu',
    title: 'Host menu from the sidebar footer',
    async run({ window }) {
      await connectedToThisMac(window);
      await window.locator(hostChip).click();
      const menu = window.locator('.quick-input-widget');
      await menu.waitFor({ state: 'visible' });
      await visible(window, '.quick-input-widget [data-quick-input-id="current"]', '.quick-input-widget [data-quick-input-id="reconnect"]');
      return screenshot(window);
    },
  },
  {
    name: 'agents-window-accounts',
    title: 'Customize, Accounts, with the bundled wispd and no CLIs installed',
    async run({ window }) {
      await connectedToThisMac(window);
      await window.locator('.wisp-threads-action', { hasText: 'Customize' }).click();
      await visible(window, '.wisp-accounts', '.wisp-accounts-section-title');
      return screenshot(window);
    },
  },
  {
    name: 'agents-window-project',
    title: 'A new project in the sidebar, with its Project tab',
    async run(context) {
      await createProject(context);
      await visible(context.window, '.part.sidebar button.wisp-threads-row.selected');
      return screenshot(context.window);
    },
  },
  {
    name: 'agents-window-project-reopened',
    title: 'The project is still listed after quitting Wisp and wispd',
    async run(context) {
      await createProject(context);
      // Both the app and wispd stop, so the project comes back from wispd's store on disk.
      const reopened = await context.relaunch();
      await connectedToThisMac(reopened.window);
      const row = reopened.window.locator(projectRow);
      await row.waitFor({ state: 'visible' });
      await row.click();
      await projectTab(reopened.window);
      return screenshot(reopened.window);
    },
  },
  {
    name: 'agents-window-context',
    title: 'Shared context added, listed, and open with its host bar',
    async run(context) {
      await createProject(context, { withCommit: true });
      await addSharedContextFile(context.window, 'notes.md');
      return screenshot(context.window);
    },
  },
  {
    name: 'agents-window-agents-panel',
    title: 'The Agents panel, opened from the pill above the coordinator',
    env: () => fakeClaudeEnv(),
    async run(context) {
      await createProject(context, { withCommit: true });
      await startSubagent(context.window);
      await chatShows(context.window, agentReply);
      await openAgentsPanel(context.window, projectName);
      await context.window.locator('.wisp-agents-panel-row[aria-label*="Needs review"]').waitFor({ state: 'visible', timeout: 30_000 });
      return screenshot(context.window);
    },
  },
  {
    name: 'agents-window-subagent',
    title: 'A subagent in a tab next to the coordinator, messaged directly',
    env: () => fakeClaudeEnv(),
    async run(context) {
      await createProject(context, { withCommit: true });
      await startSubagent(context.window);
      await chatShows(context.window, agentReply);
      await agentTab(context.window, agentTask).click();
      await sendMessage(context.window, 'also handle credit notes');
      await chatShows(context.window, 'Fake agent heard: also handle credit notes');
      return screenshot(context.window);
    },
  },
];
