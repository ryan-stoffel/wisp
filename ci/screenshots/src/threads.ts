// Normal-thread steps shared by the scenarios and smoke checks: New Chat opens upstream's new-session
// composer, and a first message starts a thread that wisp's sidebar lists under its repository.
import type { Page } from 'playwright-core';
import { pickRow } from './agents.ts';
import { projectName } from './projects.ts';

/** The first message the checks start a thread with, and the title its row shows. */
export const threadTask = 'Fix the flaky attach test';

/** Clicks New Chat once it is enabled, and waits for the new-session composer. */
export async function openNewChat(window: Page): Promise<void> {
  const action = window.locator('.part.sidebar .wisp-threads-action', { hasText: 'New Chat' }).first();
  await window.locator('.part.sidebar .wisp-threads-action:not(.disabled)', { hasText: 'New Chat' }).waitFor({ state: 'visible', timeout: 30_000 });
  await action.click();
  await composer(window).waitFor({ state: 'visible', timeout: 30_000 });
}

/** The Agents window's main part, which holds the new-session composer. */
export function sessionsPart(window: Page) {
  return window.locator('.part.sessionspart').last();
}

function composer(window: Page) {
  return sessionsPart(window).locator('.sessions-chat-editor .monaco-editor').last();
}

/** A thread's row under its repository in the sidebar's Repositories section. */
export function repoThreadRow(window: Page) {
  return window.locator(`.part.sidebar .wisp-threads-thread-section button.wisp-threads-row[aria-label^="${threadTask}, chat in ${projectName}"]`).first();
}

/**
 * Types the first message into the new-session composer and sends it. A fresh wispd has no default
 * account for agents, so it then picks Claude Code, as Start Subagent does.
 */
export async function sendFirstMessage(window: Page): Promise<void> {
  await composer(window).click();
  await window.keyboard.type(threadTask);
  await window.keyboard.press('Enter');
  await pickRow(window, 'Claude Code');
}
