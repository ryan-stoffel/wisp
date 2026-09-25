// Small helpers for driving wisp's own UI (the Agents window's sidebar, host chip, and commands),
// on top of ci/smoke's ui.ts pattern (the command palette settle window is the same 500ms, for the
// same reason: the list re-renders on a filter timer with no event to wait on).
import type { Page } from 'playwright-core';

// Matches ci/screenshots/src/scenarios.ts's own `hostChip` selector.
const HOST_CHIP = '.part.sidebar button.wisp-threads-host';
const PALETTE_INPUT = '.quick-input-widget input';

export interface HostChipStatus {
  /** `describeHostStatus`'s `kind`: connected, connecting, notConnected, or error. */
  readonly kind: string;
  /** Its `mark`: connected, connecting, idle, or error. */
  readonly mark: string;
  readonly name: string;
  readonly state: string;
  readonly ariaLabel: string;
}

export async function hostChipStatus(window: Page): Promise<HostChipStatus> {
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
  }, HOST_CHIP);
}

/** Waits until the host chip's kind (connected, connecting, notConnected, or error) is one of `kinds`. */
export async function waitForHostKind(window: Page, kinds: readonly string[], timeoutMs: number): Promise<HostChipStatus> {
  await window.waitForFunction(
    ({ selector, wanted }) => {
      const kind = document.querySelector(selector)?.getAttribute('data-kind');
      return kind !== null && kind !== undefined && wanted.includes(kind);
    },
    { selector: HOST_CHIP, wanted: kinds },
    { timeout: timeoutMs },
  );
  return hostChipStatus(window);
}

/** The sidebar's project rows, by their accessible name ("<name>, project, updated <age>"). */
export async function projectRowLabels(window: Page): Promise<string[]> {
  return window.evaluate(() =>
    [...document.querySelectorAll('.wisp-threads-rows button.wisp-threads-row')].map(
      (row) => row.getAttribute('aria-label') ?? '',
    ),
  );
}

/**
 * Runs a command through the command palette by its title, such as "Wisp: New Project...". Waits
 * for a matching row before accepting, so a filter that hasn't settled yet never runs the wrong
 * (or no) command.
 */
export async function runCommand(window: Page, title: string): Promise<void> {
  await window.keyboard.press('ControlOrMeta+Shift+KeyP');
  const input = window.locator(PALETTE_INPUT);
  await input.waitFor({ state: 'visible' });
  await input.fill(title);
  const row = window.locator('.quick-input-widget .monaco-list-row').filter({ hasText: title }).first();
  await row.waitFor({ state: 'visible' });
  await window.keyboard.press('Enter');
}

/**
 * Waits for whatever quick input the last command opened (a picker or an input box; they share
 * the same widget) and types into it, or leaves its default value if `text` is undefined.
 */
export async function fillQuickInput(window: Page, text?: string): Promise<void> {
  // The palette closes and, if the command opens another quick input, a new one replaces it in the
  // same widget; ci/smoke's paletteCommands waits out that same re-render with a fixed settle window.
  await window.waitForTimeout(500);
  const input = window.locator(PALETTE_INPUT).first();
  await input.waitFor({ state: 'visible' });
  if (text !== undefined) {
    await input.fill(text);
  }
}
