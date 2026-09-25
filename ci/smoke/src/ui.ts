// Small UI helpers shared by the checks: driving the command palette and reading what is on screen.
// Nothing here is scenario-specific; each check decides what to search for and what an empty result means.
import type { Page } from 'playwright-core';

const paletteInput = '.quick-input-widget input';

/**
 * Opens the command palette, types `>query`, and returns the visible rows whose text actually contains
 * `query` (case-insensitive). The palette itself fuzzy-matches the query as a subsequence of letters, not a
 * substring, so most queries return commands that merely happen to contain the same letters in order (a
 * 4-letter query like "chat" matches "Change Tab Display Size"); filtering again here is what makes the
 * result mean what a caller expects it to mean.
 */
export async function paletteCommands(window: Page, query: string): Promise<string[]> {
  await window.keyboard.press('ControlOrMeta+Shift+KeyP');
  const input = window.locator(paletteInput);
  await input.waitFor({ state: 'visible' });
  await input.fill(`>${query}`);
  // The list re-renders on a filter timer; there is no event to wait on, so give it a fixed, short settle
  // window instead of a growing retry loop. 500ms is well over what the filter itself takes.
  await window.waitForTimeout(500);
  const rows = await window.evaluate(() => {
    function accessibleText(el: Element): string {
      const label = el.getAttribute('aria-label');
      const text = label !== null && label !== '' ? label : el.textContent;
      return text.trim().replace(/\s+/g, ' ');
    }
    return [...document.querySelectorAll('.quick-input-widget .monaco-list-row')].map(accessibleText);
  });
  await window.keyboard.press('Escape');
  const needle = query.toLowerCase();
  return rows.filter((row) => row.toLowerCase().includes(needle));
}

/** IDs present in the DOM as an element's own `id` attribute, restricted to the given candidates. */
export async function presentElementIds(window: Page, candidates: readonly string[]): Promise<string[]> {
  return window.evaluate(
    (ids) => ids.filter((id) => document.getElementById(id) !== null),
    candidates,
  );
}

export interface StatusBarItem {
  readonly id: string;
  readonly text: string;
}

export async function statusBarItems(window: Page): Promise<StatusBarItem[]> {
  return window.evaluate(() => {
    function accessibleText(el: Element): string {
      const label = el.getAttribute('aria-label');
      const text = label !== null && label !== '' ? label : el.textContent;
      return text.trim();
    }
    return [...document.querySelectorAll<HTMLElement>('.part.statusbar .statusbar-item[id]')]
      .filter((el) => el.offsetWidth > 0 || el.offsetHeight > 0)
      .map((el) => ({ id: el.id, text: accessibleText(el) }));
  });
}

export async function titleBarText(window: Page): Promise<string[]> {
  return window.evaluate(() => {
    function accessibleText(el: Element): string {
      const label = el.getAttribute('aria-label');
      const text = label !== null && label !== '' ? label : el.textContent;
      return text.trim();
    }
    return [...document.querySelectorAll<HTMLElement>('.part.titlebar .action-item, .part.titlebar .monaco-button')]
      .filter((el) => el.offsetWidth > 0 || el.offsetHeight > 0)
      .map(accessibleText)
      .filter((text) => text.length > 0);
  });
}

export async function visibleChatElements(window: Page): Promise<string[]> {
  return window.evaluate(() =>
    [...document.querySelectorAll('.interactive-session, .chat-viewpane, .chat-widget, .inline-chat, .chat-welcome-view, [id="workbench.panel.chat"]')]
      .filter((el) => (el as HTMLElement).offsetWidth > 0 || (el as HTMLElement).offsetHeight > 0)
      .map((el) => el.id || el.className),
  );
}
