import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative } from 'node:path';
import { test } from 'node:test';
import { LIMITS, nameProblem, textProblem } from '../src/manifest.ts';
import { scenarios } from '../src/scenarios.ts';

test('ships the Agents window, startup, and editor scenarios', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.deepEqual(names.slice(0, 3), ['agents-window', 'startup', 'editor-file-open']);
});

test('names are unique and titles are text the comment accepts', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.equal(new Set(names).size, names.length);
  for (const { name, title } of scenarios) {
    assert.equal(nameProblem(name), undefined, name);
    assert.equal(textProblem(title, LIMITS.title), undefined, title);
  }
});

test('launch arguments are paths that exist inside the scenario directory', async () => {
  for (const scenario of scenarios) {
    if (!scenario.args) {
      continue;
    }
    const dir = await mkdtemp(join(tmpdir(), 'wisp-scenario-args-'));
    try {
      for (const arg of await scenario.args(dir)) {
        if (isAbsolute(arg)) {
          assert.ok(existsSync(arg), `${scenario.name}: ${arg} does not exist`);
          assert.ok(!relative(dir, arg).startsWith('..'), `${scenario.name}: ${arg} is outside ${dir}`);
        }
      }
    } finally {
      await rm(dir, { recursive: true, force: true });
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
