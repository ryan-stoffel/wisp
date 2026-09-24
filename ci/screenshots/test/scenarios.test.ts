import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { test } from 'node:test';
import { scenarios } from '../src/scenarios.ts';

test('ships the startup, editor, and coordinator chat scenarios', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.deepEqual(names.slice(0, 3), ['startup', 'editor-file-open', 'coordinator-chat']);
});

test('names are unique and safe as file names', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.equal(new Set(names).size, names.length);
  for (const name of names) {
    assert.match(name, /^[a-z0-9]+(-[a-z0-9]+)*$/);
  }
});

test('titles fit in Markdown headings and image alt text', () => {
  for (const { title } of scenarios) {
    assert.match(title, /^[^[\]\n]+$/);
  }
});

test('absolute paths in launch arguments exist', () => {
  for (const scenario of scenarios) {
    for (const arg of scenario.args ?? []) {
      if (isAbsolute(arg)) {
        assert.ok(existsSync(arg), `${scenario.name}: ${arg} does not exist`);
      }
    }
  }
});
