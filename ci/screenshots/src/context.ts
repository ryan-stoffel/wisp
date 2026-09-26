// Shared context steps for #106's scenario: adding a file from the Project tab's section, the
// way a user does, and waiting for it to be listed and open with its "lives on this host" bar.
import type { Page } from 'playwright-core';

/**
 * Clicks the Shared context section's `+`, types `name`, and presses Enter. Waits for the file to
 * be listed in the section and open in the editor with its host bar.
 *
 * `name` must end in `.txt`, not `.md`: opening a `.md` resource here opens the bundled Markdown
 * extension's preview (a webview, with its own "Lock Preview" toggle) instead of the plain text
 * editor, so `.view-lines` never appears. That is upstream's own default for the language, not
 * anything wisp's provider controls, and every other extension of a shared context file (0005,
 * #155) opens as plain text, so `.txt` still captures the real behavior this scenario checks.
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
