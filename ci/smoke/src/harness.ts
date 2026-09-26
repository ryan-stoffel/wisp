// ci/screenshots' launch code, plus what the smoke suite needs on top: a throwaway HOME (a terminal
// runs the user's shell, which reads rc files from HOME, and Source Control reads git's user config
// from it) and a git-initialized copy of the fixture workspace to open.
import { execFile } from 'node:child_process';
import { cp, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { appLaunchOptions, launch, type Session } from '../../screenshots/src/harness.ts';

const execFileAsync = promisify(execFile);
const fixtureWorkspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');

/**
 * Launches the app under a throwaway HOME, with WISP_WISPD_PATH (node/wispdService.ts) pointed at a
 * path that never exists: the checks that read the no-host view need a launch that never connects,
 * and the rest do not use wispd.
 */
export async function launchSmoke(args: readonly string[] = []): Promise<Session> {
  const options = await appLaunchOptions();
  const home = await mkdtemp(join(tmpdir(), 'wisp-smoke-home-'));
  await mkdir(join(home, '.wisp'), { recursive: true });
  const session = await launch(
    { ...options, env: { ...options.env, HOME: home, WISP_WISPD_PATH: join(home, 'no-such-wispd') } },
    { args: () => Promise.resolve(args) },
  );
  const close = async (): Promise<void> => {
    await session.close();
    await rm(home, { recursive: true, force: true, maxRetries: 3 });
  };
  return { ...session, close };
}

/** Copies the fixture workspace into a fresh temp directory and git-inits it, so Source Control has a repo. */
export async function gitWorkspace(): Promise<{ folder: string; cleanup(): Promise<void> }> {
  const root = await mkdtemp(join(tmpdir(), 'wisp-smoke-workspace-'));
  const folder = join(root, 'project');
  await cp(fixtureWorkspace, folder, { recursive: true });
  const git = (...gitArgs: string[]) =>
    execFileAsync('git', gitArgs, {
      cwd: folder,
      env: { ...process.env, HOME: root, GIT_CONFIG_NOSYSTEM: '1' },
    });
  await git('init', '-q', '-b', 'main');
  await git('-c', 'user.name=wisp smoke test', '-c', 'user.email=smoke@example.invalid', 'add', '.');
  await git('-c', 'user.name=wisp smoke test', '-c', 'user.email=smoke@example.invalid', 'commit', '-q', '-m', 'init');
  return { folder, cleanup: () => rm(root, { recursive: true, force: true, maxRetries: 3 }) };
}
