// Small helpers for driving wisp's own UI: the Agents window's sidebar, host chip, and host menu.
// The Agents window has no workspace open, so Ctrl+Shift+P there doesn't reach the ordinary
// command palette (it opens an empty Quick Open, which always says "No matching results"); wisp's
// own commands are driven through the sidebar buttons and the host chip's own menu instead, the
// way ci/screenshots/src/scenarios.ts's `agents-window-host-menu` scenario already does.
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

/** Opens the host chip's own menu (wispHostMenu.ts's `showHostMenu`), by clicking the chip. */
export async function openHostMenu(window: Page): Promise<void> {
  await window.locator(HOST_CHIP).click();
  await window.locator('.quick-input-widget').waitFor({ state: 'visible' });
}

/**
 * Clicks a host menu row by its id (`hostMenuItems`'s `current`, `local`, `add`, `reconnect`, or
 * `log`), the same `data-quick-input-id` attribute ci/screenshots/src/scenarios.ts's own
 * `agents-window-host-menu` scenario checks for. Clicking a row accepts it, the same as Enter on
 * the active item.
 */
export async function clickHostMenuItem(window: Page, id: string): Promise<void> {
  await window.locator(`.quick-input-widget [data-quick-input-id="${id}"]`).click();
}

/**
 * Waits for whatever quick input opened after clicking a host menu row (an input box; it shares
 * the widget the menu itself was in) and types into it, or leaves its default value if `text` is
 * undefined.
 */
export async function fillQuickInput(window: Page, text?: string): Promise<void> {
  // The menu closes and the row's own action opens another quick input in the same widget; ci/smoke's
  // paletteCommands waits out the same kind of re-render with a fixed settle window.
  await window.waitForTimeout(500);
  const input = window.locator(PALETTE_INPUT).first();
  await input.waitFor({ state: 'visible' });
  if (text !== undefined) {
    await input.fill(text);
  }
}
