import { cp } from 'node:fs/promises';
import { join } from 'node:path';
import type { Page } from 'playwright-core';
import { screenshot, visible, type Scenario } from './harness.ts';

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = 'tasks.ts';

/** An ssh destination that can never resolve (RFC 2606), so ssh fails at once with no route. */
const unreachableHost = 'wisp-screenshots.invalid';

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
      await visible(window, '.quick-input-widget .monaco-list-row[aria-label="Host: this Mac, connected"]');
      await menu.getByText('Reconnect', { exact: true }).waitFor({ state: 'visible' });
      return screenshot(window);
    },
  },
];
