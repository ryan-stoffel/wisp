import { execFile } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { _electron, errors, type ElectronApplication, type Locator, type Page } from 'playwright-core';

export interface ScenarioContext {
  app: ElectronApplication;
  window: Page;
}

export interface NotAvailable {
  notAvailable: string;
}

export interface Scenario {
  name: string;
  title: string;
  args?: readonly string[];
  run(context: ScenarioContext): Promise<Buffer | NotAvailable>;
}

export type LaunchOptions = NonNullable<Parameters<typeof _electron.launch>[0]>;

export interface Session extends ScenarioContext {
  close(): Promise<void>;
}

export const TIMEOUT_MS = 60_000;

const repoRoot = join(import.meta.dirname, '..', '..', '..');
const workbenchSelector = '.monaco-workbench';
const codeOssFlags = ['--skip-welcome', '--skip-release-notes', '--disable-workspace-trust'];
const execFileAsync = promisify(execFile);

export function notAvailable(reason: string): NotAvailable {
  return { notAvailable: reason };
}

export function isNotAvailable(value: Buffer | NotAvailable): value is NotAvailable {
  return !Buffer.isBuffer(value);
}

export function screenshot(window: Page): Promise<Buffer> {
  return window.screenshot({ animations: 'disabled', caret: 'hide' });
}

export async function hasWorkbench(window: Page): Promise<boolean> {
  return (await window.locator(workbenchSelector).count()) > 0;
}

export async function appears(locator: Locator, timeout: number): Promise<boolean> {
  try {
    await locator.first().waitFor({ state: 'visible', timeout });
    return true;
  } catch (error) {
    if (error instanceof errors.TimeoutError) {
      return false;
    }
    throw error;
  }
}

export async function appLaunchOptions(): Promise<LaunchOptions> {
  const script = join(repoRoot, 'scripts', 'ci', 'app-launch');
  try {
    const { stdout } = await execFileAsync(script, { encoding: 'utf8' });
    return JSON.parse(stdout) as LaunchOptions;
  } catch (error) {
    const stderr = (error as { stderr?: string }).stderr?.trim();
    throw new Error(`scripts/ci/app-launch failed${stderr ? `: ${stderr}` : ''}`, { cause: error });
  }
}

export async function launch(options: LaunchOptions, extraArgs: readonly string[]): Promise<Session> {
  const profile = await mkdtemp(join(tmpdir(), 'wisp-screenshots-'));
  let app: ElectronApplication | undefined;
  const close = async (): Promise<void> => {
    if (app) {
      const child = app.process();
      await Promise.race([app.close().catch(() => undefined), delay(10_000)]);
      if (child.exitCode === null && child.signalCode === null) {
        child.kill('SIGKILL');
      }
    }
    await rm(profile, { recursive: true, force: true, maxRetries: 3 });
  };
  try {
    app = await _electron.launch({
      ...options,
      args: [...(options.args ?? []), `--user-data-dir=${profile}`, ...codeOssFlags, ...extraArgs],
      env: { ...inheritedEnv(), ...options.env },
      timeout: TIMEOUT_MS,
    });
    const window = await app.firstWindow({ timeout: TIMEOUT_MS });
    window.setDefaultTimeout(TIMEOUT_MS);
    return { app, window, close };
  } catch (error) {
    await close();
    throw error;
  }
}

export async function ready(window: Page): Promise<void> {
  await window.waitForURL((url) => url.protocol !== 'about:');
  if (window.url().startsWith('vscode-file:')) {
    await window.locator(workbenchSelector).waitFor();
  }
  await window.evaluate(
    async ({ quietMs, limitMs }) => {
      await new Promise<void>((resolve) => {
        let quiet: ReturnType<typeof setTimeout> | undefined;
        const observer = new MutationObserver(() => {
          clearTimeout(quiet);
          quiet = setTimeout(finish, quietMs);
        });
        const limit = setTimeout(finish, limitMs);
        function finish(): void {
          observer.disconnect();
          clearTimeout(quiet);
          clearTimeout(limit);
          resolve();
        }
        quiet = setTimeout(finish, quietMs);
        observer.observe(document, { childList: true, subtree: true, characterData: true });
      });
      await document.fonts.ready;
      await Promise.race([
        new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
        new Promise((resolve) => setTimeout(resolve, 1_000)),
      ]);
    },
    { quietMs: 500, limitMs: 10_000 },
  );
}

function inheritedEnv(): Record<string, string> {
  return Object.fromEntries(
    Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );
}
