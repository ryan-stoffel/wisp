// Review Agent Changes opens the multi-diff editor on a real agent run's commit, read over wispd's
// agent/diff and agent/file, never from the worktree. The bundled wispd runs the fake `claude` as a
// worker, which writes FAKE_AGENT_NOTES.md; the check starts that run over its own `wispd attach` to
// the same data folder, so it doesn't depend on the Agents panel's flow.
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fakeClaudeEnv, pickRow, runWispCommand } from '../../../screenshots/src/agents.ts';
import { appLaunchOptions, launch, ready } from '../../../screenshots/src/harness.ts';
import { gitRepository } from '../../../screenshots/src/projects.ts';
import { check } from '../check.ts';
import { bundledWispd, uuidV7, Wispd } from '../wispd.ts';

interface Run {
  readonly id: string;
  readonly status: string;
  readonly diff?: { readonly commit: string; readonly files: number };
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
    const repo = await gitRepository(root, { withCommit: true });
    const env = { ...options.env, HOME: home, ...fakeClaudeEnv(0) };
    let step = 'launch';
    const session = await launch({ ...options, env });
    let wispd: Wispd | undefined;
    try {
      const { window } = session;
      await ready(session);

      step = "wispd runs the fake worker, which writes FAKE_AGENT_NOTES.md, and commits the run's worktree";
      wispd = await Wispd.attach(bundledWispd(options), {
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
      await runWispCommand(window, 'Review Agent Changes', 30_000);
      await pickRow(window, 'Add a review note', 30_000);

      step = 'the multi-diff editor shows FAKE_AGENT_NOTES.md with its added lines';
      const entry = window.locator('.multiDiffEntry').filter({ hasText: 'FAKE_AGENT_NOTES.md' }).first();
      await entry.waitFor({ state: 'visible', timeout: 30_000 });
      await window.waitForFunction(
        () => [...document.querySelectorAll('.multiDiffEntry .view-line')]
          .some((line) => line.textContent.replaceAll(String.fromCharCode(160), ' ').includes('Notes from the fake agent')),
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
