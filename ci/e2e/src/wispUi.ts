// Helpers for driving wisp's own UI: the Agents window's sidebar, host chip, and host menu. The
// Agents window has no workspace, so Ctrl+Shift+P there opens an empty Quick Open instead of the
// command palette; wisp's commands are driven through the sidebar and the host chip's menu instead.
import type { Page } from 'playwright-core';
import { hostChip } from '../../screenshots/src/projects.ts';

const QUICK_INPUT = '.quick-input-widget input';
const PROJECT_ROW = '.wisp-threads-rows button.wisp-threads-row';
const NEW_PROJECT_BUTTON = '.part.sidebar button.wisp-threads-new-project';

export interface HostChipStatus {
  /** connected, connecting, notConnected, or error. */
  readonly kind: string;
  readonly mark: string;
  readonly name: string;
  readonly state: string;
  readonly ariaLabel: string;
}

async function hostChipStatus(window: Page): Promise<HostChipStatus> {
  return window.evaluate((selector) => {
    const chip = document.querySelector(selector);
    if (!chip) {
      throw new Error(`no ${selector} in the DOM`);
    }
    return {
      kind: chip.getAttribute('data-kind') ?? '',
      mark: chip.getAttribute('data-mark') ?? '',
      name: chip.querySelector('.wisp-threads-host-name')?.textContent ?? '',
      state: chip.querySelector('.wisp-threads-host-state')?.textContent ?? '',
      ariaLabel: chip.getAttribute('aria-label') ?? '',
    };
  }, hostChip);
}

/** Waits until the host chip's kind is one of `kinds`. */
export async function waitForHostKind(window: Page, kinds: readonly string[], timeoutMs: number): Promise<HostChipStatus> {
  await window.waitForFunction(
    ({ selector, wanted }) => {
      const kind = document.querySelector(selector)?.getAttribute('data-kind');
      return kind !== null && kind !== undefined && wanted.includes(kind);
    },
    { selector: hostChip, wanted: kinds },
    { timeout: timeoutMs },
  );
  return hostChipStatus(window);
}

/**
 * Waits until the host chip shows both `name` and `kind`. `kind` alone races right after a host
 * switch, when the chip may still show that kind for the previous host.
 */
export async function waitForHostNamed(window: Page, name: string, kind: string, timeoutMs: number): Promise<HostChipStatus> {
  await window.waitForFunction(
    ({ selector, wantedName, wantedKind }) => {
      const chip = document.querySelector(selector);
      return chip?.getAttribute('data-kind') === wantedKind
        && chip.querySelector('.wisp-threads-host-name')?.textContent === wantedName;
    },
    { selector: hostChip, wantedName: name, wantedKind: kind },
    { timeout: timeoutMs },
  );
  return hostChipStatus(window);
}

/**
 * Waits for exactly one project row and returns its `data-session` (`wisp.project:/<id>`), read in
 * the same page evaluation that saw the row: a row added on `project/create`'s response can be
 * briefly replaced by a resync's re-list, so a second round trip could miss it. `data-session` is
 * compared rather than the row's label, whose relative age keeps changing.
 */
export async function waitForSingleProjectRow(window: Page, timeoutMs: number): Promise<string> {
  const handle = await window.waitForFunction(
    (selector) => {
      const rows = [...document.querySelectorAll(selector)];
      return rows.length === 1 ? ((rows[0] as HTMLElement).dataset.session ?? '') : undefined;
    },
    PROJECT_ROW,
    { timeout: timeoutMs },
  );
  const session = await handle.jsonValue();
  if (typeof session !== 'string' || session.length === 0) {
    throw new Error(`waitForSingleProjectRow resolved with ${JSON.stringify(session)}`);
  }
  return session;
}

/** Waits for exactly one project row, with the `data-session` already known. */
export async function waitForProjectRowSession(window: Page, expected: string, timeoutMs: number): Promise<string> {
  const handle = await window.waitForFunction(
    ({ selector, expected }) => {
      const rows = [...document.querySelectorAll(selector)];
      return rows.length === 1 && (rows[0] as HTMLElement).dataset.session === expected ? expected : undefined;
    },
    { selector: PROJECT_ROW, expected },
    { timeout: timeoutMs },
  );
  const session = await handle.jsonValue();
  if (typeof session !== 'string') {
    throw new Error(`waitForProjectRowSession resolved with ${JSON.stringify(session)}`);
  }
  return session;
}

export function projectIdFromSession(dataSession: string): string {
  return dataSession.replace(/^wisp\.project:\/?/, '');
}

/**
 * Clicks the sidebar's New Project "+" once it is enabled. It is disabled through `aria-disabled`,
 * which Playwright's actionability checks ignore, and enables only after the host connects.
 */
async function clickNewProjectButton(window: Page): Promise<void> {
  const button = window.locator(NEW_PROJECT_BUTTON);
  await button.waitFor({ state: 'visible' });
  await window.waitForFunction(
    (selector) => document.querySelector(selector)?.getAttribute('aria-disabled') !== 'true',
    NEW_PROJECT_BUTTON,
  );
  await button.click();
}

/** New Project on this Mac, for a folder already given to `answerFolderPicker`, keeping the suggested name. */
export async function createLocalProject(window: Page): Promise<void> {
  await clickNewProjectButton(window);
  const name = window.locator(QUICK_INPUT);
  await name.waitFor({ state: 'visible' });
  await name.press('Enter');
}

/**
 * New Project on a remote host, which asks for the repository's path as text: fills `path`, then
 * waits for the name step's default (the path's last segment) before accepting it too.
 */
export async function createRemoteProject(window: Page, path: string): Promise<void> {
  await clickNewProjectButton(window);
  const input = window.locator(QUICK_INPUT);
  await input.waitFor({ state: 'visible' });
  await input.fill(path);
  await window.keyboard.press('Enter');
  const suggestedName = path.split('/').filter((segment) => segment.length > 0).pop() ?? path;
  await window.waitForFunction(
    ({ selector, expected }) => document.querySelector<HTMLInputElement>(selector)?.value === expected,
    { selector: QUICK_INPUT, expected: suggestedName },
  );
  await window.keyboard.press('Enter');
}

export async function openHostMenu(window: Page): Promise<void> {
  await window.locator(hostChip).click();
  await window.locator('.quick-input-widget').waitFor({ state: 'visible' });
}

/** Clicks a host menu row by its `data-quick-input-id` (current, local, add, reconnect, or log). */
export async function clickHostMenuItem(window: Page, id: string): Promise<void> {
  await window.locator(`.quick-input-widget [data-quick-input-id="${id}"]`).click();
}

/** Waits for Add Host's input (placeholder `mac-mini`) to replace the menu, and fills it. */
export async function fillAddHostInput(window: Page, destination: string): Promise<void> {
  const input = window.locator(`${QUICK_INPUT}[placeholder="mac-mini"]`);
  await input.waitFor({ state: 'visible' });
  await input.fill(destination);
}
