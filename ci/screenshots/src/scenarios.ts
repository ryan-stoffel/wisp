import { execFile } from 'node:child_process';
import { cp } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import type { ElectronApplication, Page } from 'playwright-core';
import { screenshot, visible, type Scenario, type ScenarioContext } from './harness.ts';

const execFileAsync = promisify(execFile);

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = 'tasks.ts';

/**
 * An ssh destination nothing listens on, so ssh fails at once with "connection refused" (no
 * route). An unresolvable name can take a DNS timeout of 30 s or more, longer than the handshake.
 */
const unreachableHost = 'ssh://127.0.0.1:9';

const hostChip = '.part.sidebar button.wisp-threads-host';

/** Waits for the sidebar's host chip to reach a state, then checks its accessible name. */
async function hostChipIn(window: Page, kind: string, ariaLabel: string): Promise<void> {
  const chip = window.locator(`${hostChip}[data-kind="${kind}"]`);
  await chip.waitFor({ state: 'visible' });
  const label = await chip.getAttribute('aria-label');
  if (label !== ariaLabel) {
    throw new Error(`the host chip is labeled ${JSON.stringify(label)}, not ${JSON.stringify(ariaLabel)}`);
  }
}

/** Waits for the bundled wispd to connect: the chip says so and the no-host view goes away. */
async function connectedToThisMac(window: Page): Promise<void> {
  await visible(window, '.part.titlebar', '.part.sidebar .wisp-threads');
  await hostChipIn(window, 'connected', 'Host: this Mac, connected');
  await window.locator('.wisp-agents-no-host').waitFor({ state: 'hidden' });
}

/** The project the project scenarios create, from a git repository of the same name. */
const projectName = 'billing-service';
const projectRow = `.part.sidebar button.wisp-threads-row[aria-label^="${projectName}, project"]`;

/** Makes a real git repository in the scenario's folder, as `project/create` requires one. */
async function gitRepository(dir: string): Promise<string> {
  const repo = join(dir, projectName);
  await execFileAsync('git', ['init', '--quiet', '--initial-branch=main', repo]);
  return repo;
}

/**
 * Answers the next native folder picker with `folder`. On this Mac, New Project asks for the
 * repository with the system's folder picker, which Playwright can't drive.
 */
async function answerFolderPicker(app: ElectronApplication, folder: string): Promise<void> {
  // Electron's types aren't a dependency here, so the one method replaced is typed by hand.
  interface Dialog {
    showOpenDialog: () => Promise<{ canceled: boolean; filePaths: string[] }>;
  }
  await app.evaluate((electron: unknown, path: string) => {
    (electron as { dialog: Dialog }).dialog.showOpenDialog = () => Promise.resolve({ canceled: false, filePaths: [path] });
  }, folder);
}

/**
 * Creates a project the way a user does: `+` under Projects, the folder picker, then Enter on the
 * name wisp suggests. Waits for its row, and for its Project tab.
 */
async function createProject({ app, window, dir }: ScenarioContext): Promise<void> {
  await connectedToThisMac(window);
  await answerFolderPicker(app, await gitRepository(dir));
  await window.locator('.part.sidebar button.wisp-threads-new-project').click();
  const name = window.locator('.quick-input-widget input');
  await name.waitFor({ state: 'visible' });
  const suggested = await name.inputValue();
  if (suggested !== projectName) {
    throw new Error(`New Project suggests the name ${JSON.stringify(suggested)}, not the folder's`);
  }
  await name.press('Enter');
  await window.locator(projectRow).waitFor({ state: 'visible' });
  await projectTab(window);
}

/** Waits for the Project tab to show the project, with the repository as its first fact. */
async function projectTab(window: Page): Promise<void> {
  await window.locator('.part.auxiliarybar .wisp-project-name', { hasText: projectName }).waitFor({ state: 'visible' });
  const repo = await window.locator('.part.auxiliarybar .wisp-project-fact[data-fact="repo"] .wisp-project-fact-detail').textContent();
  if (!repo?.includes(`${projectName} on this Mac, branch main`)) {
    throw new Error(`the Project tab's repository fact is ${JSON.stringify(repo)}`);
  }
}

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
];
