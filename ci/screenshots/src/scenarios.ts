import { basename, join } from 'node:path';
import { appears, hasWorkbench, notAvailable, screenshot, type Scenario } from './harness.ts';

const workspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');
const openFile = join(workspace, 'src', 'tasks.ts');

export const scenarios: readonly Scenario[] = [
  {
    name: 'startup',
    title: 'Startup',
    run: ({ window }) => screenshot(window),
  },
  {
    name: 'editor-file-open',
    title: 'Editor with a file open',
    args: [workspace, openFile],
    async run({ window }) {
      if (!(await hasWorkbench(window))) {
        return notAvailable('needs the Code - OSS workbench, which replaces the stand-in Electron app');
      }
      await window.locator(`.monaco-editor[data-uri$="/src/${basename(openFile)}"]`).waitFor();
      return screenshot(window);
    },
  },
  {
    name: 'coordinator-chat',
    title: 'Coordinator chat',
    async run({ window }) {
      if (!(await hasWorkbench(window)) || !(await appears(window.locator('.wisp-coordinator-chat'), 10_000))) {
        return notAvailable('needs the coordinator chat view');
      }
      return screenshot(window);
    },
  },
  {
    name: 'broken-on-purpose',
    title: 'Broken on purpose',
    async run({ window }) {
      await window.locator('.does-not-exist').waitFor({ timeout: 2_000 });
      return screenshot(window);
    },
  },
];
