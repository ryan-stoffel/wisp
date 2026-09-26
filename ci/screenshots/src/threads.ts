// Normal-thread steps for the scenarios and smoke checks of #110: New Chat opens upstream's
// new-session composer, and a first message starts a thread that wisp's sidebar lists under its
// repository in Repositories. The agent is the fake Claude Code of ./agents.ts.
import type { Page } from 'playwright-core';
import { projectName } from './projects.ts';

/** The first message the checks start a thread with, and the title its row shows. */
export const threadTask = 'Fix the flaky attach test';

/** The sidebar's New Chat action. */
export function newChatAction(window: Page) {
  return window.locator('.part.sidebar .wisp-threads-action', { hasText: 'New Chat' }).first();
}

/** Clicks New Chat once it is enabled, and waits for the new-session composer. */
export async function openNewChat(window: Page): Promise<void> {
  const action = newChatAction(window);
  await window.locator('.part.sidebar .wisp-threads-action:not(.disabled)', { hasText: 'New Chat' }).waitFor({ state: 'visible', timeout: 30_000 });
  await action.click();
  await composer(window).waitFor({ state: 'visible', timeout: 30_000 });
}

/** The new-session composer's editor. */
export function composer(window: Page) {
  return window.locator('.part.sessions .monaco-editor .view-lines').last();
}

/** A thread's row under its repository in the sidebar's Repositories section. */
export function repoThreadRow(window: Page, task = threadTask, repo = projectName) {
  return window.locator(`.part.sidebar .wisp-threads-thread-section button.wisp-threads-row[aria-label^="${task}, chat in ${repo}"]`).first();
}

/**
 * Types the first message into the new-session composer and sends it. A fresh wispd has no default
 * account for agents, so it then picks Claude Code, as Start Subagent does.
 */
export async function sendFirstMessage(window: Page, text = threadTask): Promise<void> {
  await composer(window).click();
  await window.keyboard.type(text);
  await window.keyboard.press('Enter');
  const row = window.locator('.quick-input-widget .monaco-list-row').filter({ hasText: 'Claude Code' }).first();
  await row.waitFor({ state: 'visible', timeout: 20_000 });
  await row.click();
}
