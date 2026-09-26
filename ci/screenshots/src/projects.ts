// The Agents window's host and project steps that several scenarios and smoke checks share: waiting for
// the bundled wispd to connect, and creating a project the way a user does.
import { execFile } from 'node:child_process';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import type { ElectronApplication, Page } from 'playwright-core';
import { visible, type ScenarioContext } from './harness.ts';

const execFileAsync = promisify(execFile);

export const hostChip = '.part.sidebar button.wisp-threads-host';

/** Waits for the sidebar's host chip to reach a state, then checks its accessible name. */
export async function hostChipIn(window: Page, kind: string, ariaLabel: string): Promise<void> {
  const chip = window.locator(`${hostChip}[data-kind="${kind}"]`);
  await chip.waitFor({ state: 'visible' });
  const label = await chip.getAttribute('aria-label');
  if (label !== ariaLabel) {
    throw new Error(`the host chip is labeled ${JSON.stringify(label)}, not ${JSON.stringify(ariaLabel)}`);
  }
}

/** Waits for the bundled wispd to connect: the chip says so and the no-host view goes away. */
export async function connectedToThisMac(window: Page): Promise<void> {
  await visible(window, '.part.titlebar', '.part.sidebar .wisp-threads');
  await hostChipIn(window, 'connected', 'Host: this Mac, connected');
  await window.locator('.wisp-agents-no-host').waitFor({ state: 'hidden' });
}

/** The project the project scenarios create, from a git repository of the same name. */
export const projectName = 'billing-service';
export const projectRow = `.part.sidebar button.wisp-threads-row[aria-label^="${projectName}, project"]`;

export interface RepositoryOptions {
  /**
   * Gives the repository a first commit and its own git identity, which an agent's worktree needs:
   * wispd branches the worktree from HEAD and commits the agent's changes there (decision record 0014).
   */
  readonly withCommit?: boolean;
}

/** Makes a real git repository in the scenario's folder, as `project/create` requires one. */
export async function gitRepository(dir: string, options: RepositoryOptions = {}): Promise<string> {
  const repo = join(dir, projectName);
  await execFileAsync('git', ['init', '--quiet', '--initial-branch=main', repo]);
  if (options.withCommit) {
    const git = (...args: string[]) => execFileAsync('git', ['-C', repo, ...args]);
    await git('config', 'user.name', 'wisp smoke test');
    await git('config', 'user.email', 'smoke@example.invalid');
    await writeFile(join(repo, 'README.md'), '# billing-service\n');
    await git('add', 'README.md');
    await git('commit', '--quiet', '--message', 'init');
  }
  return repo;
}

/**
 * Answers the next native folder picker with `folder`. On this Mac, New Project asks for the
 * repository with the system's folder picker, which Playwright can't drive.
 */
export async function answerFolderPicker(app: ElectronApplication, folder: string): Promise<void> {
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
export async function createProject({ app, window, dir }: Pick<ScenarioContext, 'app' | 'window' | 'dir'>, options: RepositoryOptions = {}): Promise<void> {
  await connectedToThisMac(window);
  await answerFolderPicker(app, await gitRepository(dir, options));
  await window.locator('.part.sidebar button.wisp-threads-new-project').click();
  const name = window.locator('.quick-input-widget input');
  await name.waitFor({ state: 'visible' });
  const suggested = await name.inputValue();
  if (suggested !== projectName) {
    throw new Error(`New Project suggests the name ${JSON.stringify(suggested)}, not the folder's`);
  }
  await name.press('Enter');
  await window.locator(projectRow).waitFor({ state: 'visible' });
  try {
    await projectTab(window);
  } catch (error) {
    console.log(await projectTabDiagnostics(app, window));
    throw error;
  }
}

/**
 * What the window and its logs say when the Project tab never shows a new project, for a flake
 * that only a loaded runner hits (#183): where focus is, whether the row is selected, any
 * notification, and the renderer's and wispd's recent log lines.
 */
async function projectTabDiagnostics(app: ElectronApplication, window: Page): Promise<string> {
  const lines = ['Project tab diagnostics (#183):'];
  try {
    const dom = await window.evaluate(() => ({
      focused: document.activeElement ? `${document.activeElement.tagName}.${document.activeElement.className} ${document.activeElement.getAttribute('aria-label') ?? ''}`.slice(0, 200) : null,
      rows: [...document.querySelectorAll('.part.sidebar button.wisp-threads-row')].map((row) => `${row.getAttribute('aria-label') ?? ''}${row.classList.contains('selected') ? ' (selected)' : ''}`),
      notifications: document.querySelector('.notifications-toasts')?.textContent.slice(0, 500) ?? null,
      auxiliaryBar: document.querySelector('.part.auxiliarybar')?.textContent.slice(0, 200) ?? null,
    }));
    lines.push(JSON.stringify(dom));
    // Code - OSS keeps a folder of logs per launch under the profile; the newest is this app's.
    const userData = await app.evaluate((electron: unknown) => (electron as { app: { getPath(name: string): string } }).app.getPath('userData'));
    const launches = (await readdir(join(userData, 'logs'))).sort();
    const logs = join(userData, 'logs', launches[launches.length - 1] ?? '');
    for (const file of (await readdir(logs, { recursive: true })).sort()) {
      if (file.endsWith('renderer.log') || file === 'wispd.log' || file === 'sharedprocess.log') {
        const text = await readFile(join(logs, file), 'utf8');
        const kept = text.split('\n').filter((line) => file === 'wispd.log' || /\[(error|warning)\]|wisp|SessionsView/.test(line));
        lines.push(`--- ${file}, last ${String(Math.min(kept.length, 40))} of ${String(kept.length)} lines kept`, ...kept.slice(-40));
      }
    }
  } catch (error) {
    lines.push(`(diagnostics failed: ${String(error)})`);
  }
  return lines.join('\n');
}

/** Waits for the Project tab to show the project, with the repository as its first fact. */
export async function projectTab(window: Page): Promise<void> {
  await window.locator('.part.auxiliarybar .wisp-project-name', { hasText: projectName }).waitFor({ state: 'visible' });
  const repo = await window.locator('.part.auxiliarybar .wisp-project-fact[data-fact="repo"] .wisp-project-fact-detail').textContent();
  if (!repo?.includes(`${projectName} on this Mac, branch main`)) {
    throw new Error(`the Project tab's repository fact is ${JSON.stringify(repo)}`);
  }
}
