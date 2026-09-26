// Thin wrapper over ci/screenshots' launch code, the way ci/smoke's own harness.ts is (see its
// comment for why: node resolves "playwright-core" relative to whichever file imports it, so a
// value import in screenshots/src/harness.ts needs that package's own node_modules regardless of
// who calls it). Unlike ci/smoke, e2e wants the bundled wispd wired up exactly as a plain launch
// leaves it (0010): `launch` here is screenshots' own, unwrapped, so every check gets a real
// connection unless it asks for something else (a fake wispd, mid-session).
import { spawn } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
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

export interface SshLaunch {
  readonly session: Session;
  /** The data dir the ssh-side wispd (started by `ssh localhost ... attach`) uses; see below. */
  readonly sshWispdDataDir: string;
  /** The bundled wispd's own path, for stopping the process holding `sshWispdDataDir`'s lock. */
  readonly wispdPath: string;
  /** The wrapper script's own path (what `wisp.remoteWispdPath` was set to). */
  readonly wrapperPath: string;
  /** Stops the ssh-side wispd (it runs on this same machine, so no ssh is needed to reach it) and removes its data dir. */
  close(): Promise<void>;
}

/**
 * Launches connected, with `wisp.remoteWispdPath` pointed at a small wrapper script that sets
 * `WISPD_DATA_DIR` to a fresh temp dir and execs the bundled wispd. Two problems this solves at
 * once: a bare GitHub Actions runner has no Homebrew `wispd` on `PATH` for `ssh localhost` to find
 * on itself (daemon/README.md's PATH gotcha), and `ssh` never forwards `WISPD_DATA_DIR` (sshd's
 * `AcceptEnv` only allows `LANG`/`LC_*`), so without the wrapper the ssh-side wispd would use the
 * runner user's real default data dir. Pointing `wisp.remoteWispdPath` at a nonstandard binary is
 * exactly what a real host would do (0007); wrapping it to fix the data dir is this test's own
 * addition, invisible to the editor.
 */
export async function launchConnectedForSsh(): Promise<SshLaunch> {
  const options = await appLaunchOptions();
  const wispdPath = bundledWispdPath(options);
  const sshWispdDataDir = await mkdtemp(join(tmpdir(), 'wisp-e2e-ssh-wispd-'));
  const wrapper = join(sshWispdDataDir, 'wispd-wrapper.sh');
  await writeFile(
    wrapper,
    `#!/bin/sh\nexport WISPD_DATA_DIR='${sshWispdDataDir}'\nexec '${wispdPath}' "$@"\n`,
  );
  await chmod(wrapper, 0o755);
  const session = await launch(options, {
    args: () => Promise.resolve([]),
    settings: { 'wisp.remoteWispdPath': wrapper },
  });
  return {
    session,
    sshWispdDataDir,
    wispdPath,
    wrapperPath: wrapper,
    close: async () => {
      await stopWispdAt(sshWispdDataDir);
      await rm(sshWispdDataDir, { recursive: true, force: true, maxRetries: 3 });
    },
  };
}

/** A workspace of a project's own repository, git-initialized (ci/smoke's own fixture). */
export function projectWorkspace(): Promise<Workspace> {
  return gitWorkspace();
}

/** `SIGKILL`s the `wispd serve` holding `dataDir`'s lock, if any, and waits for it to exit. */
async function stopWispdAt(dataDir: string): Promise<void> {
  const pid = await readWispdPid(dataDir);
  if (pid === undefined) {
    return;
  }
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    return;
  }
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

/**
 * `SIGKILL`s the `wispd serve` a session's bundled wispd started, without touching the app itself,
 * for the mid-session reconnect check. Returns once the process is gone (or was never found).
 */
export async function killWispd(session: Session): Promise<void> {
  const pid = await readWispdPid(session.wispdDataDir);
  if (pid === undefined) {
    throw new Error(`no wispd.lock in ${session.wispdDataDir}; was wispd ever connected?`);
  }
  await stopWispdAt(session.wispdDataDir);
}

/**
 * Sends `initialize` then `project/list` to an already-spawned `wispd attach` (or an ssh session
 * running one) and returns the projects `project/list` answers with. Shared by
 * {@link queryProjectsDirect} and {@link queryProjectsOverSsh}.
 */
async function queryProjects(child: {
  readonly stdin: import('node:stream').Writable;
  readonly stdout: import('node:stream').Readable;
  kill(): boolean;
}): Promise<readonly { id: string }[]> {
  try {
    const request = (id: number, method: string, params: unknown) =>
      `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`;
    child.stdin.write(request(1, 'initialize', { protocol: { min: 1, max: 1 }, client: { name: 'ci-e2e-query', version: '0' }, capabilities: {} }));
    child.stdin.write(request(2, 'project/list', {}));
    child.stdin.end();

    const lines: string[] = [];
    const rl = createInterface({ input: child.stdout });
    for await (const line of rl) {
      lines.push(line);
      if (lines.length >= 2) {
        break;
      }
    }
    rl.close();

    const responses = lines.map((line) => JSON.parse(line) as { id: number; result?: { projects?: { id: string }[] }; error?: unknown });
    const listResponse = responses.find((response) => response.id === 2);
    if (!listResponse || listResponse.error || !listResponse.result?.projects) {
      throw new Error(`no project/list result from wispd: ${lines.join(' | ')}`);
    }
    return listResponse.result.projects;
  } finally {
    child.kill();
  }
}

/**
 * Asks a wispd directly for its projects, bypassing the editor entirely: spawns `<wispdExecutable>
 * attach` against `dataDir`, and reads back `project/list`. Ground truth for "no duplicate project
 * after a reconnect" (reconnect.ts): `WispProjectsService` keeps its old list across a reconnect
 * until the resubscribe's `resync` lands and it re-lists, so reading the UI right after the chip
 * reconnects can see the stale array. wispd's own store has no such lag. Also used by ssh.ts to
 * confirm this Mac's own wispd (session.wispdDataDir) does *not* hold a project created over ssh.
 */
export function queryProjectsDirect(wispdExecutable: string, dataDir: string): Promise<readonly { id: string }[]> {
  const child = spawn(wispdExecutable, ['attach'], {
    env: { ...process.env, WISPD_DATA_DIR: dataDir },
    stdio: ['pipe', 'pipe', 'ignore'],
  });
  return queryProjects(child);
}

/**
 * The same, but over `ssh localhost` (ssh.ts), through the same wrapper `wisp.remoteWispdPath`
 * points at (`launchConnectedForSsh`), so this reaches the identical `WISPD_DATA_DIR` the editor's
 * own ssh session does.
 *
 * Ground truth for ssh.ts's project-creation check: before #219's fix, every CI run that used this
 * (or an equivalent local `attach` against the ssh-side `WISPD_DATA_DIR`) found the project
 * missing, even right after the sidebar showed it -- a race that sent `project/create` to the old
 * host's wispd instead. #219 fixed that race, and #224 re-enabled this assertion in ssh.ts.
 */
export function queryProjectsOverSsh(wrapperPath: string, destination = 'localhost'): Promise<readonly { id: string }[]> {
  const child = spawn(
    'ssh',
    ['-T', '-o', 'BatchMode=yes', '-o', 'ControlPath=none', '--', destination, `${wrapperPath} attach`],
    { stdio: ['pipe', 'pipe', 'ignore'] },
  );
  return queryProjects(child);
}

/** ci.yml points this at a folder to save into for `saveWispdDiagnostics`; unset for a local run. */
const DIAGNOSTICS_DIR_ENV = 'WISP_E2E_DIAGNOSTICS_DIR';

/**
 * Best-effort snapshot for a failed ground-truth check (ssh.ts): each named data folder's own
 * `logs/wispd.log` (daemon/src/paths.rs), plus a listing of every `wispd serve` process on this
 * machine with the data folder its own environment names (`ps eww`, since the data folder is
 * passed as the `WISPD_DATA_DIR` environment variable, not a command-line argument -- see
 * `DataDir::command`), so a failure has more to go on than "it was empty". Saved under a
 * timestamped subfolder so more than one failure in a run keeps all of them. Does nothing unless
 * `ci.yml`'s `e2e` job has set `WISP_E2E_DIAGNOSTICS_DIR`, and never throws: a diagnostics step
 * that fails shouldn't hide the assertion failure that triggered it.
 */
export async function saveWispdDiagnostics(dataDirs: Readonly<Record<string, string>>): Promise<void> {
  const dir = process.env[DIAGNOSTICS_DIR_ENV];
  if (!dir) {
    return;
  }
  try {
    const out = join(dir, `wispd-${String(Date.now())}`);
    await mkdir(out, { recursive: true });
    for (const [label, dataDir] of Object.entries(dataDirs)) {
      try {
        await writeFile(join(out, `${label}.log`), await readFile(join(dataDir, 'logs', 'wispd.log')));
      } catch {
        // no log file yet for this data folder; nothing to save.
      }
    }
    await writeFile(join(out, 'wispd-serve-processes.txt'), await listWispdServeProcesses());
  } catch {
    // diagnostics are best-effort.
  }
}

/** `ps`'s own listing of every `wispd serve` process, with the data folder from its environment. */
async function listWispdServeProcesses(): Promise<string> {
  const matches = (await runCommand('ps', ['-axo', 'pid=,command=']))
    .split('\n')
    .map((line) => /^\s*(\d+)\s+(.*\bwispd\b.*\bserve\b.*)$/.exec(line))
    .filter((match): match is RegExpExecArray => match !== null);
  if (matches.length === 0) {
    return '(no wispd serve processes found)\n';
  }
  const lines = await Promise.all(
    matches.map(async (match) => {
      const pid = match[1] ?? '';
      const command = match[2] ?? '';
      const env = await runCommand('ps', ['eww', '-p', pid, '-o', 'command=']);
      const dataDir = /WISPD_DATA_DIR=(\S+)/.exec(env)?.[1] ?? '(unknown)';
      return `pid ${pid}: ${dataDir} -- ${command}`;
    }),
  );
  return `${lines.join('\n')}\n`;
}

/** Runs `command` and resolves with its stdout, or `''` on any error (diagnostics are best-effort). */
function runCommand(command: string, args: readonly string[]): Promise<string> {
  return new Promise((resolve) => {
    let output = '';
    try {
      const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'ignore'] });
      child.stdout.on('data', (chunk: Buffer) => {
        output += chunk.toString();
      });
      child.on('close', () => {
        resolve(output);
      });
      child.on('error', () => {
        resolve(output);
      });
    } catch {
      resolve(output);
    }
  });
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
