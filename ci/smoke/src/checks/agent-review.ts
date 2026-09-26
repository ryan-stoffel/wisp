// Review Agent Changes (#157) opens the multi-diff editor on a real agent run's commit, read over
// wispd's agent/diff and agent/file through the wisp-agent: file system, never from the worktree.
// The run is real too: the packaged app's own wispd runs the fake `claude` (fixtures/fake-cli) as a
// worker, which writes REVIEW.md, and wispd commits it. The check starts that run over its own
// `wispd attach` to the same data folder, since starting runs from the Agents window is #105's.
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { promisify } from 'node:util';
import { launch, type LaunchOptions } from '../../../screenshots/src/harness.ts';
import { check } from '../check.ts';
import { appLaunchOptions, ready } from '../harness.ts';
import { bundledWispd, uuidV7, Wispd } from '../wispd.ts';

const execFileAsync = promisify(execFile);
const fakeCliDir = join(import.meta.dirname, '..', '..', 'fixtures', 'fake-cli');

interface Run {
  readonly id: string;
  readonly status: string;
  readonly diff?: { readonly commit: string; readonly files: number };
}

/** A repository with one commit and its own identity, as a user's checkout would be. */
async function repository(root: string): Promise<string> {
  const repo = join(root, 'app');
  const git = (...args: string[]) => execFileAsync('git', ['-C', repo, ...args], { env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1' } });
  await execFileAsync('git', ['init', '-q', '-b', 'main', repo]);
  await git('config', 'user.name', 'wisp smoke test');
  await git('config', 'user.email', 'smoke@example.invalid');
  await writeFile(join(repo, 'README.md'), '# App\n');
  await git('add', '-A');
  await git('commit', '-q', '-m', 'init');
  return repo;
}

async function finishedRun(wispd: Wispd, runId: string): Promise<Run> {
  const deadline = Date.now() + 60_000;
  let last: Run | undefined;
  while (Date.now() < deadline) {
    const { runs } = await wispd.request<{ runs: Run[] }>('agent/list', {});
    last = runs.find((run) => run.id === runId);
    if (last && last.status !== 'starting' && last.status !== 'running') {
      return last;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`the run never finished: ${JSON.stringify(last)}`);
}

export const agentReviewChecks = [
  check('Review Agent Changes opens a diff of a fake run from wispd', async () => {
    const options = await appLaunchOptions();
    const root = await mkdtemp(join(tmpdir(), 'wisp-smoke-review-'));
    const home = join(root, 'home');
    await mkdir(home);
    const repo = await repository(root);
    const env = {
      ...options.env,
      HOME: home,
      PATH: `${fakeCliDir}${delimiter}${process.env.PATH ?? ''}`,
    };
    const withFakeCli: LaunchOptions = { ...options, env };
    let step = 'launch';
    const session = await launch(withFakeCli);
    let wispd: Wispd | undefined;
    try {
      const { window } = session;
      await ready(session);

      step = "wispd runs the fake worker, which writes REVIEW.md, and commits the run's worktree";
      if (!options.executablePath) {
        throw new Error('app-launch gave no executablePath');
      }
      wispd = await Wispd.attach(bundledWispd(options.executablePath), {
        ...process.env,
        ...env,
        WISPD_DATA_DIR: session.wispdDataDir,
      });
      const project = uuidV7();
      await wispd.request('project/create', { id: project, name: 'app', repoPath: repo });
      const runId = uuidV7();
      await wispd.request('agent/start', {
        runId,
        project,
        prompt: 'Add a review note',
        policy: 'workspaceWrite',
        account: { kind: 'subscription', backend: 'claude' },
      });
      const run = await finishedRun(wispd, runId);
      if (run.status !== 'completed' || run.diff?.files !== 1) {
        throw new Error(`expected a completed run with one changed file, got ${JSON.stringify(run)}`);
      }

      step = 'Wisp: Review Agent Changes lists the run';
      await window.keyboard.press('ControlOrMeta+Shift+KeyP');
      const input = window.locator('.quick-input-widget input');
      await input.waitFor({ state: 'visible' });
      await input.fill('>Wisp: Review Agent Changes');
      const command = window
        .locator('.quick-input-widget .monaco-list-row')
        .filter({ hasText: 'Review Agent Changes' })
        .first();
      await command.waitFor({ state: 'visible', timeout: 30_000 });
      await command.click();
      const pick = window
        .locator('.quick-input-widget .monaco-list-row')
        .filter({ hasText: 'Add a review note' })
        .first();
      await pick.waitFor({ state: 'visible', timeout: 30_000 });
      await pick.click();

      step = 'the multi-diff editor shows REVIEW.md with its added lines';
      const entry = window.locator('.multiDiffEntry').filter({ hasText: 'REVIEW.md' }).first();
      await entry.waitFor({ state: 'visible', timeout: 30_000 });
      await window.waitForFunction(
        () => [...document.querySelectorAll('.multiDiffEntry .view-line')]
          .some((line) => line.textContent.replaceAll(String.fromCharCode(160), ' ').includes('Written by the smoke test')),
        undefined,
        { timeout: 30_000 },
      );
      const title = await window.locator('.tab.active').first().textContent();
      if (!title?.includes('Review: Add a review note')) {
        throw new Error(`expected the editor tab to be titled "Review: Add a review note", got ${JSON.stringify(title)}`);
      }
    } catch (error) {
      throw new Error(`${step}: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
    } finally {
      wispd?.close();
      await session.close();
      await rm(root, { recursive: true, force: true, maxRetries: 3 });
    }
  }),
];
