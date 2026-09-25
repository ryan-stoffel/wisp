import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
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
  args?: (dir: string) => Promise<readonly string[]>;
  run(context: ScenarioContext): Promise<Buffer | NotAvailable>;
}

export type LaunchOptions = NonNullable<Parameters<typeof _electron.launch>[0]>;

export interface Session extends ScenarioContext {
  close(): Promise<void>;
}

export const TIMEOUT_MS = 60_000;
export const WINDOW_SIZE = { width: 1024, height: 640 };

const repoRoot = join(import.meta.dirname, '..', '..', '..');
const workbenchSelector = '.monaco-workbench';
const workbenchRestoredMark = 'code/didStartWorkbench';
const appFlags = ['--skip-welcome', '--skip-release-notes', '--disable-workspace-trust', '--use-inmemory-secretstorage'];
const execFileAsync = promisify(execFile);

export function notAvailable(reason: string): NotAvailable {
  return { notAvailable: reason };
}

export function isNotAvailable(value: Buffer | NotAvailable): value is NotAvailable {
  return !Buffer.isBuffer(value);
}

export function screenshot(window: Page, timeout = TIMEOUT_MS): Promise<Buffer> {
  return window.screenshot({ animations: 'disabled', caret: 'hide', timeout });
}

export async function visible(window: Page, ...selectors: string[]): Promise<void> {
  for (const selector of selectors) {
    await window.locator(selector).first().waitFor({ state: 'visible' });
  }
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

export async function launch(options: LaunchOptions, scenarioArgs?: Scenario['args']): Promise<Session> {
  const root = await mkdtemp(join(tmpdir(), 'wisp-screenshots-'));
  let app: ElectronApplication | undefined;
  const close = async (): Promise<void> => {
    if (app) {
      const child = app.process();
      await Promise.race([app.close().catch(() => undefined), delay(10_000)]);
      if (child.exitCode === null && child.signalCode === null) {
        child.kill('SIGKILL');
      }
    }
    await rm(root, { recursive: true, force: true, maxRetries: 3 });
  };
  try {
    const files = join(root, 'files');
    await mkdir(files);
    const extraArgs = (await scenarioArgs?.(files)) ?? [];
    app = await _electron.launch({
      ...options,
      args: [
        ...(options.args ?? []),
        `--user-data-dir=${join(root, 'user-data')}`,
        `--extensions-dir=${join(root, 'extensions')}`,
        ...appFlags,
        ...extraArgs,
      ],
      env: { ...inheritedEnv(), ...options.env },
      timeout: TIMEOUT_MS,
    });
    const window = await firstRealWindow(app);
    window.setDefaultTimeout(TIMEOUT_MS);
    return { app, window, close };
  } catch (error) {
    await close();
    throw error;
  }
}

const entryPointUrl = /\/(workbench|sessions)\.html(?:[?#]|$)/;

// app.firstWindow() trusts whichever BrowserWindow Electron creates first, at whatever URL it has at that
// instant (usually still about:blank). On a cold launch that first window can be a transient page that closes
// again before the caller gets to it, losing the window Playwright was watching (#13's Progress comment on a
// flake #10 saw once). Instead, wait for a window whose navigation actually reaches the app's own HTML; a
// transient window's predicate simply times out and Playwright keeps waiting for the real one. One wait, no retry.
async function firstRealWindow(app: ElectronApplication): Promise<Page> {
  return app.waitForEvent('window', {
    timeout: TIMEOUT_MS,
    predicate: async (page) => {
      // Any failure here (a timeout, or the page closing first) means this particular window never
      // became the real one; let Playwright keep waiting instead of failing the whole wait on it.
      return page
        .waitForURL(entryPointUrl, { timeout: TIMEOUT_MS })
        .then(() => true)
        .catch(() => false);
    },
  });
}

export async function ready({ app, window }: ScenarioContext): Promise<void> {
  await window.waitForURL((url) => url.protocol !== 'about:');
  await fitWindow(app, window);
  await window.locator(workbenchSelector).waitFor();
  await window.waitForFunction((mark) => performance.getEntriesByName(mark, 'mark').length > 0, workbenchRestoredMark);
  await window.evaluate(
    async ({ quietMs, limitMs, fontsMs }) => {
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
      const atMost = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
      await Promise.race([document.fonts.ready, atMost(fontsMs)]);
      await Promise.race([
        new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
        atMost(1_000),
      ]);
    },
    { quietMs: 500, limitMs: 10_000, fontsMs: 5_000 },
  );
}

async function fitWindow(app: ElectronApplication, window: Page): Promise<void> {
  const browserWindow = await app.browserWindow(window);
  await browserWindow.evaluate(
    (win: { setContentSize(width: number, height: number): void }, size) => {
      win.setContentSize(size.width, size.height);
    },
    WINDOW_SIZE,
  );
  try {
    await window.waitForFunction(
      ({ width, height }) => innerWidth === width && innerHeight === height,
      WINDOW_SIZE,
      { timeout: 5_000 },
    );
  } catch (error) {
    if (!(error instanceof errors.TimeoutError)) {
      throw error;
    }
    const actual = await window.evaluate(() => `${String(innerWidth)}x${String(innerHeight)}`);
    console.log(`window content is ${actual}, not ${String(WINDOW_SIZE.width)}x${String(WINDOW_SIZE.height)}`);
  }
}

function inheritedEnv(): Record<string, string> {
  return Object.fromEntries(
    Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );
}
