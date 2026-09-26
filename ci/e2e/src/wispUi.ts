// Small helpers for driving wisp's own UI: the Agents window's sidebar, host chip, and host menu.
// The Agents window has no workspace open, so Ctrl+Shift+P there doesn't reach the ordinary
// command palette (it opens an empty Quick Open, which always says "No matching results"); wisp's
// own commands are driven through the sidebar buttons and the host chip's own menu instead, the
// way ci/screenshots/src/scenarios.ts's `agents-window-host-menu` scenario already does.
import type { Page } from 'playwright-core';

// Matches ci/screenshots/src/scenarios.ts's own `hostChip` selector.
const HOST_CHIP = '.part.sidebar button.wisp-threads-host';
const QUICK_INPUT = '.quick-input-widget input';

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

/**
 * Waits until the host chip shows both `name` and `kind`. Checking `kind` alone races when the
 * chip already shows that kind for the *previous* host (for example, waiting for 'connected' right
 * after switching away from an already-connected "this Mac" can see the old, still-connected chip
 * and return immediately, before the switch has taken effect).
 */
export async function waitForHostNamed(window: Page, name: string, kind: string, timeoutMs: number): Promise<HostChipStatus> {
  await window.waitForFunction(
    ({ selector, wantedName, wantedKind }) => {
      const chip = document.querySelector(selector);
      return chip?.getAttribute('data-kind') === wantedKind
        && chip.querySelector('.wisp-threads-host-name')?.textContent === wantedName;
    },
    { selector: HOST_CHIP, wantedName: name, wantedKind: kind },
    { timeout: timeoutMs },
  );
  return hostChipStatus(window);
}

/**
 * The sidebar's project rows, by their `data-session` (the project's session resource, e.g.
 * `wisp.project:/<id>`; see wispProjects.ts's `projectResource`). Stable across a relaunch or a
 * reconnect, unlike the accessible label wispThreadsView.ts builds for each row: that label ends
 * in a relative age ("...updated now", then "...updated N secs ago" after 30 s, ticking every
 * second), so comparing labels exactly can fail on a slow run for reasons that have nothing to do
 * with the project list itself.
 */
export async function projectRowSessions(window: Page): Promise<string[]> {
  return window.evaluate(() =>
    [...document.querySelectorAll('.wisp-threads-rows button.wisp-threads-row')].map(
      (row) => (row as HTMLElement).dataset.session ?? '',
    ),
  );
}

/** The project id inside a row's `data-session` (`wisp.project:/<id>`), or the raw value if it doesn't match. */
export function projectIdFromSession(dataSession: string): string {
  return dataSession.replace(/^wisp\.project:\/?/, '');
}

const NEW_PROJECT_BUTTON = '.part.sidebar button.wisp-threads-new-project';

/**
 * Clicks the sidebar's New Project "+", first waiting for it to actually be enabled.
 * `wispThreadsView.ts`'s `enableable` disables a button through `aria-disabled`, not the native
 * `disabled` attribute (screen readers announce the reason, which a real `disabled` button can't
 * carry), so Playwright's own actionability checks -- which wait on the native attribute, not
 * `aria-disabled` -- happily click it while it is still disabled, whose handler then does nothing.
 * The button becomes enabled reactively once the host connects; on a slow or loaded runner, that
 * autorun can lag behind the chip's own DOM update by more than an instant.
 */
export async function clickNewProjectButton(window: Page): Promise<void> {
  const button = window.locator(NEW_PROJECT_BUTTON);
  await button.waitFor({ state: 'visible' });
  await window.waitForFunction(
    (selector) => document.querySelector(selector)?.getAttribute('aria-disabled') !== 'true',
    NEW_PROJECT_BUTTON,
  );
  await button.click();
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
 * Waits for wispHostMenu.ts's `addHost` input (`placeHolder: 'mac-mini'`) to replace the menu, and
 * fills it with `destination`. That placeholder is the deterministic signal that the menu closed
 * and its own input box is up, rather than a fixed wait that could still be looking at the menu's
 * own filter box if the box hasn't swapped in yet.
 */
export async function fillAddHostInput(window: Page, destination: string): Promise<void> {
  const input = window.locator(`${QUICK_INPUT}[placeholder="mac-mini"]`);
  await input.waitFor({ state: 'visible' });
  await input.fill(destination);
}

/**
 * Runs New Project (the sidebar's "+") for a remote host, where wispNewProject.ts's flow asks for
 * the repository's path as text (`askPath`) instead of the native folder picker: fills `path`,
 * accepts it, waits for the name step's own default value (the path's last segment, exactly what
 * `askName(basename(path))` pre-fills), then accepts that too. Waiting for the specific default
 * value is the deterministic signal that the path step's input was replaced by the name step's,
 * rather than a fixed wait that could still be looking at the path box.
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
