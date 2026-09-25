import { cp } from 'node:fs/promises';
import { join } from 'node:path';
import { appears, notAvailable, screenshot, visible, type Scenario } from './harness.ts';

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = 'tasks.ts';

export const scenarios: readonly Scenario[] = [
  {
    name: 'agents-window',
    title: 'Agents window at startup',
    async run({ window }) {
      await visible(window, '.part.titlebar', '.part.sessionspart');
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
    name: 'coordinator-chat',
    title: 'Coordinator chat',
    async run({ window }) {
      if (!(await appears(window.locator('.wisp-coordinator-chat'), 10_000))) {
        return notAvailable('needs the coordinator chat view');
      }
      return screenshot(window);
    },
  },
];
