// The Agents window's host and project steps that several scenarios and smoke checks share: waiting for
// the bundled wispd to connect, and creating a project the way a user does.
import { execFile } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
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
  await projectTab(window);
}

/** Waits for the Project tab to show the project, with the repository as its first fact. */
export async function projectTab(window: Page): Promise<void> {
  await window.locator('.part.auxiliarybar .wisp-project-name', { hasText: projectName }).waitFor({ state: 'visible' });
  const repo = await window.locator('.part.auxiliarybar .wisp-project-fact[data-fact="repo"] .wisp-project-fact-detail').textContent();
  if (!repo?.includes(`${projectName} on this Mac, branch main`)) {
    throw new Error(`the Project tab's repository fact is ${JSON.stringify(repo)}`);
  }
}
