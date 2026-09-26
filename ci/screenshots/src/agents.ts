// Subagent steps for the scenarios and smoke checks of #105: start one on a project with the developer
// command, open the Agents panel from the pill, open a subagent's tab, and message it. The agent is
// ci/smoke/fixtures/fake-cli/claude, put first on PATH, so the app's own wispd runs it as Claude Code:
// it writes a file in its worktree and answers a follow-up by echoing it.
import { delimiter, join } from 'node:path';
import type { Page } from 'playwright-core';

const fakeCliDir = join(import.meta.dirname, '..', '..', 'smoke', 'fixtures', 'fake-cli');

/** The task the checks start a subagent with, and the title its tab and row show. */
export const agentTask = 'Render invoice PDFs from Stripe line items';
/** What the fake agent says once it has written its file. */
export const agentReply = 'Fake agent wrote FAKE_AGENT_NOTES.md.';

/** The environment that puts the fake `claude` on PATH, for wispd's detection and its workers. */
export function fakeClaudeEnv(delaySeconds = 2): Record<string, string> {
  return {
    PATH: `${fakeCliDir}${delimiter}${process.env.PATH ?? ''}`,
    FAKE_CLAUDE_DELAY: String(delaySeconds),
  };
}

/** The palette's rows whose text contains `text`, once the list shows one. */
async function pickRow(window: Page, text: string): Promise<void> {
  const row = window.locator('.quick-input-widget .monaco-list-row').filter({ hasText: text }).first();
  await row.waitFor({ state: 'visible', timeout: 20_000 });
  await row.click();
}

/**
 * Runs **Wisp: Start Subagent** on the open project: the task, then, since a fresh wispd has no
 * default account for agents, Claude Code. Waits for the subagent's tab, which opens on its own.
 */
export async function startSubagent(window: Page, task = agentTask): Promise<void> {
  await window.keyboard.press('ControlOrMeta+Shift+KeyP');
  const input = window.locator('.quick-input-widget input');
  await input.waitFor({ state: 'visible' });
  await input.fill('>Wisp: Start Subagent');
  await pickRow(window, 'Start Subagent');
  await input.waitFor({ state: 'visible' });
  await input.fill(task);
  await input.press('Enter');
  await pickRow(window, 'Claude Code');
  await agentTab(window, task).waitFor({ state: 'visible', timeout: 30_000 });
}

/** A chat tab next to the coordinator, by its title's start. */
export function agentTab(window: Page, task = agentTask) {
  return window.locator('[role="tab"]').filter({ hasText: task.slice(0, 16) }).first();
}

/** Waits until the visible chat shows `text`. */
export async function chatShows(window: Page, text: string, timeout = 60_000): Promise<void> {
  await window.locator('.interactive-session .interactive-item-container').filter({ hasText: text }).first().waitFor({ state: 'visible', timeout });
}

/** The Agents pill above the coordinator's composer. */
export function agentsPill(window: Page) {
  return window.locator('.chat-pill-button[aria-label^="Agents,"]').first();
}

/** Switches to the coordinator's tab, then opens the Agents panel from the pill. */
export async function openAgentsPanel(window: Page, project: string): Promise<void> {
  await window.locator('[role="tab"]').filter({ hasText: project }).first().click();
  const pill = agentsPill(window);
  await pill.waitFor({ state: 'visible', timeout: 30_000 });
  await pill.click();
  await window.locator('.wisp-agents-panel').waitFor({ state: 'visible' });
}

/** Types a message into the visible composer and sends it with Enter. */
export async function sendMessage(window: Page, text: string): Promise<void> {
  await window.locator('.interactive-input-part .monaco-editor .view-lines').last().click();
  await window.keyboard.type(text);
  await window.keyboard.press('Enter');
}
