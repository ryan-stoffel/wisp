import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, isAbsolute, join, relative } from 'node:path';
import { test } from 'node:test';
import { LIMITS, nameProblem, textProblem } from '../src/manifest.ts';
import { scenarios } from '../src/scenarios.ts';

test('ships the Agents window, startup, and editor scenarios', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.deepEqual(names.slice(0, 3), ['agents-window', 'startup', 'editor-file-open']);
});

test('captures the host connected, unreachable, and its menu', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.ok(names.includes('agents-window-disconnected'));
  assert.ok(names.includes('agents-window-host-menu'));
  const disconnected = scenarios.find((scenario) => scenario.name === 'agents-window-disconnected');
  assert.match(String(disconnected?.settings?.['wisp.host']), /^ssh:\/\/127\.0\.0\.1:\d+$/, 'a closed local port, so ssh fails fast');
});

test('creates a project, and sees it again after quitting Wisp and wispd', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.ok(names.includes('agents-window-project'));
  assert.ok(names.includes('agents-window-project-reopened'));
});

test('captures the Agents panel and a subagent tab, with the fake claude first on PATH', () => {
  for (const name of ['agents-window-agents-panel', 'agents-window-subagent']) {
    const scenario = scenarios.find((candidate) => candidate.name === name);
    assert.ok(scenario, name);
    const path = scenario.env?.('/unused').PATH ?? '';
    assert.match(path.split(delimiter)[0] ?? '', /ci\/smoke\/fixtures\/fake-cli$/, name);
  }
});

test('captures New Chat, and a normal thread under Repositories with the fake claude first on PATH', () => {
  const names = scenarios.map((scenario) => scenario.name);

  assert.ok(names.includes('agents-window-new-chat'));
  const thread = scenarios.find((candidate) => candidate.name === 'agents-window-repo-thread');
  assert.ok(thread);
  const path = thread.env?.('/unused').PATH ?? '';
  assert.match(path.split(delimiter)[0] ?? '', /ci\/smoke\/fixtures\/fake-cli$/);
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
