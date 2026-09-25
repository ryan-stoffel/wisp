import { cp } from 'node:fs/promises';
import { join } from 'node:path';
import { screenshot, visible, type Scenario } from './harness.ts';

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = 'tasks.ts';

export const scenarios: readonly Scenario[] = [
  {
    name: 'agents-window',
    title: 'Agents window at startup',
    async run({ window }) {
      // With no host connected, the no-host view covers the session surface (#12).
      await visible(window, '.part.titlebar', '.part.sidebar .wisp-threads', '.wisp-agents-no-host');
      const input = window.locator('.wisp-agents-no-host textarea');
      const send = window.locator('.wisp-agents-no-host .wisp-no-host-send');
      const state = {
        readonly: await input.evaluate((element) => (element as HTMLTextAreaElement).readOnly),
        inputAriaDisabled: await input.getAttribute('aria-disabled'),
        sendAriaDisabled: await send.getAttribute('aria-disabled'),
      };
      if (!state.readonly || state.inputAriaDisabled !== 'true' || state.sendAriaDisabled !== 'true') {
        throw new Error(`the no-host composer is not disabled: ${JSON.stringify(state)}`);
      }
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
];
