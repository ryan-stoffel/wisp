// Shared context steps for #106's scenario: adding a file from the Project tab's section, the
// way a user does, and waiting for it to be listed and open with its "lives on this host" bar.
import type { Page } from 'playwright-core';

/**
 * Clicks the Shared context section's `+`, types `name`, and presses Enter. Waits for the file to
 * be listed in the section and open in the editor with its host bar.
 */
export async function addSharedContextFile(window: Page, name: string): Promise<void> {
  await window.locator('.wisp-project-context-add').click();
  const input = window.locator('.quick-input-widget input');
  await input.waitFor({ state: 'visible' });
  await input.fill(name);
  await input.press('Enter');
  await window.locator('.wisp-project-context-link', { hasText: name }).waitFor({ state: 'visible' });
  await window.locator(`.monaco-editor[data-uri$="/${name}"] .view-lines`).waitFor({ state: 'visible' });
  await window.locator('.wisp-context-banner').waitFor({ state: 'visible' });
}
