// Launch helpers over ci/screenshots' harness. Unlike ci/smoke, e2e keeps the bundled wispd wired
// up as a plain launch leaves it (0010), so every check gets a real connection unless it asks for
// something else.
import { execFile } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { appLaunchOptions, launch, readWispdPid, type Session } from '../../screenshots/src/harness.ts';
import { bundledWispd, Wispd } from '../../smoke/src/wispd.ts';

const execFileAsync = promisify(execFile);

/** A plain launch: the packaged app, its bundled wispd, and a throwaway everything else. */
export async function launchConnected(): Promise<Session> {
  return launch(await appLaunchOptions());
}

/** Launches with `executable` run as `<executable> attach` in place of the bundled wispd. */
export async function launchWithWispd(executable: string): Promise<Session> {
  const options = await appLaunchOptions();
  // The editor's override for which wispd it runs (platform/wisp/node/wispdService.ts).
  return launch({ ...options, env: { ...options.env, WISP_WISPD_PATH: executable } });
}

export interface SshLaunch {
  readonly session: Session;
  /** The data folder of the ssh-side wispd that `ssh localhost ... attach` starts. */
  readonly sshWispdDataDir: string;
  readonly wispdPath: string;
  /** What `wisp.remoteWispdPath` is set to. */
  readonly wrapperPath: string;
  /** Stops the ssh-side wispd and removes its data folder. */
  close(): Promise<void>;
}

/**
 * Launches connected, with `wisp.remoteWispdPath` pointed at a wrapper that sets `WISPD_DATA_DIR`
 * to a fresh temp folder and execs the bundled wispd. A bare runner has no `wispd` on the PATH
 * that `ssh localhost` sees, and ssh never forwards `WISPD_DATA_DIR` (sshd's `AcceptEnv`), so
 * without the wrapper the ssh-side wispd would use the runner user's real data folder.
 */
export async function launchConnectedForSsh(): Promise<SshLaunch> {
  const options = await appLaunchOptions();
  const wispdPath = bundledWispd(options);
  const sshWispdDataDir = await mkdtemp(join(tmpdir(), 'wisp-e2e-ssh-wispd-'));
  const wrapperPath = join(sshWispdDataDir, 'wispd-wrapper.sh');
  await writeFile(wrapperPath, `#!/bin/sh\nexport WISPD_DATA_DIR='${sshWispdDataDir}'\nexec '${wispdPath}' "$@"\n`);
  await chmod(wrapperPath, 0o755);
  const session = await launch(options, { settings: { 'wisp.remoteWispdPath': wrapperPath } });
  return {
    session,
    sshWispdDataDir,
    wispdPath,
    wrapperPath,
    close: async () => {
      await stopWispdAt(sshWispdDataDir);
      await rm(sshWispdDataDir, { recursive: true, force: true, maxRetries: 3 });
    },
  };
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

/** `SIGKILL`s the `wispd serve` a session started, without touching the app itself. */
export async function killWispd(session: Session): Promise<void> {
  if ((await readWispdPid(session.wispdDataDir)) === undefined) {
    throw new Error(`no wispd.lock in ${session.wispdDataDir}; was wispd ever connected?`);
  }
  await stopWispdAt(session.wispdDataDir);
}

type Projects = readonly { id: string }[];

async function listProjects(command: string, env: NodeJS.ProcessEnv, args?: readonly string[]): Promise<Projects> {
  const wispd = await Wispd.attach(command, env, args);
  try {
    return (await wispd.request<{ projects: Projects }>('project/list', {})).projects;
  } finally {
    wispd.close();
  }
}

/**
 * The projects in a wispd's own store, asked over a second `wispd attach` that bypasses the editor:
 * ground truth where the editor's list can lag, such as right after a reconnect.
 */
export function queryProjectsDirect(wispdExecutable: string, dataDir: string): Promise<Projects> {
  return listProjects(wispdExecutable, { ...process.env, WISPD_DATA_DIR: dataDir });
}

/** The same over `ssh localhost`, through the wrapper `wisp.remoteWispdPath` points at. */
export function queryProjectsOverSsh(wrapperPath: string): Promise<Projects> {
  return listProjects('ssh', process.env, ['-T', '-o', 'BatchMode=yes', '-o', 'ControlPath=none', '--', 'localhost', `${wrapperPath} attach`]);
}

/**
 * Best-effort snapshot for a failed ground-truth check, into `WISP_E2E_DIAGNOSTICS_DIR` (set by
 * ci.yml's e2e job, which uploads it): each data folder's `logs/wispd.log`, and every `wispd serve`
 * process with the data folder from its environment. Never throws, so it cannot hide the failure
 * that triggered it.
 */
export async function saveWispdDiagnostics(dataDirs: Readonly<Record<string, string>>): Promise<void> {
  const dir = process.env.WISP_E2E_DIAGNOSTICS_DIR;
  if (!dir) {
    return;
  }
  try {
    const out = join(dir, `wispd-${String(Date.now())}`);
    await mkdir(out, { recursive: true });
    for (const [label, dataDir] of Object.entries(dataDirs)) {
      await readFile(join(dataDir, 'logs', 'wispd.log')).then((log) => writeFile(join(out, `${label}.log`), log), () => undefined);
    }
    await writeFile(join(out, 'wispd-serve-processes.txt'), await listWispdServeProcesses());
  } catch {
    // Best-effort.
  }
}

async function listWispdServeProcesses(): Promise<string> {
  const matches = [...(await output('ps', ['-axo', 'pid=,command='])).matchAll(/^\s*(\d+)\s+(.*\bwispd\b.*\bserve\b.*)$/gm)];
  if (matches.length === 0) {
    return '(no wispd serve processes found)\n';
  }
  const lines = await Promise.all(
    matches.map(async ([, pid = '', command = '']) => {
      // The data folder is in serve's environment, not its arguments.
      const env = await output('ps', ['eww', '-p', pid, '-o', 'command=']);
      return `pid ${pid}: ${/WISPD_DATA_DIR=(\S+)/.exec(env)?.[1] ?? '(unknown)'} -- ${command}`;
    }),
  );
  return `${lines.join('\n')}\n`;
}

function output(command: string, args: readonly string[]): Promise<string> {
  return execFileAsync(command, args).then(({ stdout }) => stdout, () => '');
}

/** Reads a small `KEY=value` file, such as the one `scripts/ci/ssh-localhost` writes. */
export async function readEnvFile(path: string): Promise<Record<string, string>> {
  const text = await readFile(path, 'utf8').catch(() => '');
  const entries: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const equals = line.indexOf('=');
    if (equals !== -1) {
      entries[line.slice(0, equals).trim()] = line.slice(equals + 1).trim();
    }
  }
  return entries;
}
