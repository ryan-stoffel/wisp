// Thin wrapper over ci/screenshots' launch code (../../screenshots/src/harness.ts), reused rather than
// duplicated. The smoke suite needs two things screenshots does not: a throwaway HOME on every launch (a
// terminal test spawns the user's real shell, which reads rc files from HOME, and Source Control reads git's
// user config from it) and a git-initialized copy of the fixture workspace to open.
import { execFile } from 'node:child_process';
import { cp, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { appLaunchOptions, launch as launchApp, type LaunchOptions, type Session } from '../../screenshots/src/harness.ts';

export {
  TIMEOUT_MS,
  WINDOW_SIZE,
  appears,
  appLaunchOptions,
  ready,
  screenshot,
  visible,
  type LaunchOptions,
  type ScenarioContext,
  type Session,
} from '../../screenshots/src/harness.ts';

const execFileAsync = promisify(execFile);
const fixtureWorkspace = join(import.meta.dirname, '..', 'fixtures', 'workspace');

export interface SmokeSession extends Session {
  readonly home: string;
}

/**
 * `wisp.host` defaults to `local`, and the packaged app bundles a working `wispd`, so a plain launch
 * now reaches a real local host (#65) instead of staying in the disconnected state it always used to be
 * in. Pointing WISP_WISPD_PATH (node/wispdService.ts) at a path that can never exist keeps every launch
 * disconnected deterministically: the checks that read the no-host view need that state on purpose, and
 * the rest do not depend on wispd at all, so there is nothing to lose by keeping it out of every launch.
 */
const WISPD_PATH_ENV = 'WISP_WISPD_PATH';

/**
 * Launches the app the same way ci/screenshots does, but under a throwaway HOME in addition to the
 * throwaway user-data and extensions directories launchApp already makes, and with `args` resolved up
 * front instead of lazily, since smoke checks need the launched app, not a screenshot of it.
 */
export async function launchSmoke(args: readonly string[] = []): Promise<SmokeSession> {
  const options = await appLaunchOptions();
  const home = await mkdtemp(join(tmpdir(), 'wisp-smoke-home-'));
  await mkdir(join(home, '.wisp'), { recursive: true });
  const withHome: LaunchOptions = {
    ...options,
    env: { ...options.env, HOME: home, [WISPD_PATH_ENV]: join(home, 'no-such-wispd') },
  };
  const session = await launchApp(withHome, { args: () => Promise.resolve(args) });
  const close = async (): Promise<void> => {
    await session.close();
    await rm(home, { recursive: true, force: true, maxRetries: 3 });
  };
  return { ...session, close, home };
}

export interface Workspace {
  readonly folder: string;
  cleanup(): Promise<void>;
}

/** Copies the fixture workspace into a fresh temp directory and git-inits it, so Source Control has a repo. */
export async function gitWorkspace(): Promise<Workspace> {
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
