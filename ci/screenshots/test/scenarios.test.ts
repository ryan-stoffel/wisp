import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { test } from 'node:test';
import { LIMITS, nameProblem, textProblem } from '../src/manifest.ts';
import { scenarios } from '../src/scenarios.ts';

test('ships the startup, editor, and coordinator chat scenarios', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.deepEqual(names.slice(0, 3), ['startup', 'editor-file-open', 'coordinator-chat']);
});

test('names are unique and titles are text the comment accepts', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.equal(new Set(names).size, names.length);
  for (const { name, title } of scenarios) {
    assert.equal(nameProblem(name), undefined, name);
    assert.equal(textProblem(title, LIMITS.title), undefined, title);
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

test('the publish job imports only Node built-ins, so it runs without npm install', async () => {
  const publishPath = ['publish.ts', 'artifact.ts', 'branch.ts', 'comment.ts', 'manifest.ts'];
  for (const file of publishPath) {
    const source = await readFile(join(import.meta.dirname, '..', 'src', file), 'utf8');
    for (const [, specifier = ''] of source.matchAll(/^import[^'"]*['"]([^'"]+)['"]/gm)) {
      assert.ok(
        specifier.startsWith('node:') || publishPath.includes(specifier.replace(/^\.\//, '')),
        `${file} imports ${specifier}`,
      );
    }
  }
});
