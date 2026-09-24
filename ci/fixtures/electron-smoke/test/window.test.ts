import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { PAGE_PATH, WINDOW_TITLE, windowOptions } from '../src/window.ts';

test('window is titled wisp', () => {
  assert.equal(WINDOW_TITLE, 'wisp');
  assert.equal(windowOptions().title, WINDOW_TITLE);
});

test('renderer is sandboxed and isolated from Node', () => {
  assert.deepEqual(windowOptions().webPreferences, {
    contextIsolation: true,
    nodeIntegration: false,
    sandbox: true,
  });
});

test('page is titled wisp and says it is the CI fixture', async () => {
  const html = await readFile(PAGE_PATH, 'utf8');

  assert.match(html, /<title>wisp<\/title>/);
  assert.match(html, /CI fixture/);
});
