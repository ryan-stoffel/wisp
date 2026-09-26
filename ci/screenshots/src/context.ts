// Shared context steps for #106's scenario: adding a file from the Project tab's section, the
// way a user does, and waiting for it to be listed and open with its "lives on this host" bar.
import type { Page } from 'playwright-core';

/**
 * Clicks the Shared context section's `+`, types `name`, and presses Enter. Waits for the file to
 * be listed in the section and open in the editor with its host bar, then checks the bar neither
 * overlaps the file's own content nor clips its own message.
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
  // The banner is a fixed three-line zone (#106's third review), so line 1 of the file should always
  // render below it without needing to wait for anything more than the elements existing. Checked
  // here as a real assertion, not just a screenshot to eyeball, since a dynamic height once passed
  // this same "visible" wait while still drawing line 1 over the bar's own text.
  await window.waitForFunction((path) => {
    const editor = document.querySelector(`.monaco-editor[data-uri$="${path}"]`);
    const banner = document.querySelector('.wisp-context-banner');
    const firstLine = editor?.querySelector('.view-line');
    if (!banner || !firstLine) {
      return false;
    }
    return firstLine.getBoundingClientRect().top >= banner.getBoundingClientRect().bottom;
  }, `/${name}`);
  // No overlap doesn't rule out the opposite failure: the message itself clipped short inside its
  // own box (#106's fourth review, where "every agent... reads it" was cut off even though line 1
  // of the file correctly stayed below the bar). scrollHeight above clientHeight means the text
  // needs more room than the zone gives its wrapper, so something is being hidden.
  const clip = await window.evaluate(() => {
    const text = document.querySelector('.wisp-context-banner-text');
    return text && { scrollHeight: text.scrollHeight, clientHeight: text.clientHeight };
  });
  if (!clip) {
    throw new Error('the shared context banner has no .wisp-context-banner-text to check for clipping');
  }
  if (clip.scrollHeight > clip.clientHeight) {
    throw new Error(
      `the shared context banner clips its text: scrollHeight ${String(clip.scrollHeight)} > clientHeight ${String(clip.clientHeight)}`,
    );
  }
}
