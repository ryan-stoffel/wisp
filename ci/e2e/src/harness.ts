// Thin wrapper over ci/screenshots' launch code, the way ci/smoke's own harness.ts is (see its
// comment for why: node resolves "playwright-core" relative to whichever file imports it, so a
// value import in screenshots/src/harness.ts needs that package's own node_modules regardless of
// who calls it). Unlike ci/smoke, e2e wants the bundled wispd wired up exactly as a plain launch
// leaves it (0010): `launch` here is screenshots' own, unwrapped, so every check gets a real
// connection unless it asks for something else (a fake wispd, mid-session).
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import {
  appLaunchOptions,
  launch,
  readWispdPid,
  type Session,
} from '../../screenshots/src/harness.ts';
import { gitWorkspace, type Workspace } from '../../smoke/src/harness.ts';

export {
  TIMEOUT_MS,
  appLaunchOptions,
  launch,
  readWispdPid,
  ready,
  screenshot,
  visible,
  type LaunchOptions,
  type Session,
} from '../../screenshots/src/harness.ts';
export { gitWorkspace, type Workspace } from '../../smoke/src/harness.ts';

/** The editor's override for which `wispd` it runs (platform/wisp/node/wispdService.ts). */
const WISPD_PATH_ENV = 'WISP_WISPD_PATH';

/** A plain launch: the packaged app, its bundled wispd, and a throwaway everything else. */
export async function launchConnected(args: readonly string[] = []): Promise<Session> {
  const options = await appLaunchOptions();
  return launch(options, { args: () => Promise.resolve(args) });
}

/**
 * Launches with `executable` in place of the bundled wispd (platform/wisp/node/wispdService.ts's
 * `resolveWispdExecutable`), for the version-mismatch check's fake wispd. `executable` is run as
 * `<executable> attach`, exactly as the bundled one would be.
 */
export async function launchWithWispd(executable: string): Promise<Session> {
  const options = await appLaunchOptions();
  return launch(
    { ...options, env: { ...options.env, [WISPD_PATH_ENV]: executable } },
    { args: () => Promise.resolve([]) },
  );
}

/**
 * The bundled wispd's own absolute path (daemon/README.md), from `scripts/ci/app-launch`'s
 * `executablePath` (`<bundle>/Contents/MacOS/<exe>`): three levels up is the `.app` itself.
 */
export function bundledWispdPath(options: { executablePath?: string }): string {
  if (!options.executablePath) {
    throw new Error('appLaunchOptions() did not set executablePath (see scripts/ci/app-launch)');
  }
  const bundle = dirname(dirname(dirname(options.executablePath)));
  return join(bundle, 'Contents', 'Resources', 'app', 'bin', 'wispd');
}

/**
 * Launches connected, with `wisp.remoteWispdPath` pre-set to the bundled wispd's own path. A bare
 * GitHub Actions runner has no Homebrew `wispd` on `PATH` for the ssh check's `ssh localhost` to
 * find (daemon/README.md's PATH gotcha applies to `ssh localhost` on itself too), and this setting
 * is exactly what a real host with a nonstandard wispd location would set (0007).
 */
export async function launchConnectedForSsh(): Promise<Session> {
  const options = await appLaunchOptions();
  return launch(options, {
    args: () => Promise.resolve([]),
    settings: { 'wisp.remoteWispdPath': bundledWispdPath(options) },
  });
}

/** A workspace of a project's own repository, git-initialized (ci/smoke's own fixture). */
export function projectWorkspace(): Promise<Workspace> {
  return gitWorkspace();
}

/**
 * `SIGKILL`s the `wispd serve` a session's bundled wispd started, without touching the app itself,
 * for the mid-session reconnect check. Returns once the process is gone (or was never found).
 */
export async function killWispd(session: Session): Promise<void> {
  const pid = await readWispdPid(session.wispdDataDir);
  if (pid === undefined) {
    throw new Error(`no wispd.lock in ${session.wispdDataDir}; was wispd ever connected?`);
  }
  process.kill(pid, 'SIGKILL');
  for (let i = 0; i < 100; i++) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await delay(100);
  }
  throw new Error(`wispd (pid ${String(pid)}) did not exit after SIGKILL`);
}

/** Reads a small `KEY=value` file, such as one `scripts/ci/ssh-localhost` wrote. */
export async function readEnvFile(path: string): Promise<Record<string, string>> {
  let text: string;
  try {
    text = await readFile(path, 'utf8');
  } catch {
    return {};
  }
  const entries: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const equals = line.indexOf('=');
    if (equals === -1) {
      continue;
    }
    entries[line.slice(0, equals).trim()] = line.slice(equals + 1).trim();
  }
  return entries;
}
