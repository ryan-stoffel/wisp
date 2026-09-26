// `wisp <folder>` opens the editor window. One launch drives the file tree, editing, search, Source
// Control, a terminal, and the "no Chat UI" check in sequence, since a launch per step would
// multiply the job's runtime.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ready } from '../../../screenshots/src/harness.ts';
import { check } from '../check.ts';
import { gitWorkspace, launchSmoke } from '../harness.ts';
import { paletteCommands, visibleChatElements } from '../ui.ts';

export const editorChecks = [
  check(
    'wisp <folder> opens the editor: the file tree, editing and saving, search, Source Control, and a terminal',
    async () => {
      const workspace = await gitWorkspace();
      const session = await launchSmoke([workspace.folder]);
      let step = 'launch';
      try {
        const { window } = session;
        await ready(session);

        step = 'explorer opens a file from the tree';
        const explorer = window.locator('.explorer-folders-view');
        await explorer.waitFor({ state: 'visible' });
        await explorer.getByText('src', { exact: true }).click();
        await explorer.getByText('tasks.ts', { exact: true }).click();
        const editor = window.locator('.monaco-editor[data-uri$="/src/tasks.ts"]');
        await editor.waitFor({ state: 'visible' });

        step = 'editing and saving writes the file to disk';
        await editor.locator('.view-lines').click();
        await window.keyboard.press('ControlOrMeta+ArrowDown');
        await window.keyboard.type('\nexport const edited = true;\n');
        await window.keyboard.press('ControlOrMeta+KeyS');
        await window.waitForFunction(
          () => !document.querySelector('.tabs-container .tab.active.dirty'),
          undefined,
          { timeout: 10_000 },
        );
        const saved = readFileSync(join(workspace.folder, 'src', 'tasks.ts'), 'utf8');
        if (!saved.includes('export const edited = true;')) {
          throw new Error('the file on disk does not have the edit');
        }

        step = 'search finds the text in the folder';
        await window.keyboard.press('ControlOrMeta+Shift+KeyF');
        const searchInput = window.locator('.search-view .search-widget textarea, .search-view .search-widget input').first();
        await searchInput.waitFor({ state: 'visible' });
        await searchInput.fill('searchNeedle');
        await window.keyboard.press('Enter');
        const match = window.locator('.search-view .monaco-list-row').filter({ hasText: 'tasks.ts' }).first();
        await match.waitFor({ state: 'visible', timeout: 20_000 });

        step = 'Source Control lists the edited file';
        await window.keyboard.press('Control+Shift+KeyG');
        const scmRow = window.locator('.scm-view .monaco-list-row').filter({ hasText: 'tasks.ts' }).first();
        await scmRow.waitFor({ state: 'visible', timeout: 20_000 });

        step = 'a terminal runs a command';
        const proofFile = join(workspace.folder, '..', 'terminal-proof.txt');
        await window.keyboard.press('Control+Backquote');
        const terminal = window.locator('.terminal-wrapper .xterm, .terminal .xterm').first();
        await terminal.waitFor({ state: 'visible', timeout: 20_000 });
        await terminal.click();
        await window.keyboard.type(`echo wisp-$((40+2)) > '${proofFile}'`);
        await window.keyboard.press('Enter');
        const deadline = Date.now() + 15_000;
        while (!existsSync(proofFile) && Date.now() < deadline) {
          await window.waitForTimeout(250);
        }
        if (!existsSync(proofFile) || readFileSync(proofFile, 'utf8').trim() !== 'wisp-42') {
          throw new Error(`the terminal never produced ${proofFile}`);
        }

        step = 'no Chat UI, and the command palette offers no chat, Copilot, or sign-in commands';
        const chat = await visibleChatElements(window);
        if (chat.length > 0) {
          throw new Error(`editor window shows Chat UI: ${chat.join(', ')}`);
        }
        await editor.locator('.view-lines').click();
        await window.keyboard.press('Control+Meta+KeyI');
        await window.waitForTimeout(1000);
        const afterChord = await visibleChatElements(window);
        if (afterChord.length > 0) {
          throw new Error(`Ctrl+Cmd+I opened Chat UI: ${afterChord.join(', ')}`);
        }
        await window.keyboard.press('Escape');
        const hits: string[] = [];
        for (const term of ['chat', 'copilot', 'sign in']) {
          const rows = await paletteCommands(window, term);
          hits.push(...rows.map((row) => `${term}: ${row}`));
        }
        if (hits.length > 0) {
          throw new Error(`command palette offers chat, Copilot, or sign-in commands: ${hits.join(' | ')}`);
        }
      } catch (error) {
        throw new Error(`${step}: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
      } finally {
        await session.close();
        await workspace.cleanup();
      }
    },
  ),
];
