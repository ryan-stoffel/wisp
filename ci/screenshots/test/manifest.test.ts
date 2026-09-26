import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseManifest, textProblem } from '../src/manifest.ts';

const valid = {
  results: [
    { name: 'startup', title: 'Startup', status: 'captured', file: 'startup.png' },
    { name: 'chat', title: 'Chat', status: 'failed', error: 'Timeout #12 @someone <b>', file: 'chat.failed.png' },
    { name: 'later', title: 'Later', status: 'pending' },
  ],
};

function withResult(result: Record<string, unknown>): unknown {
  return { results: [result] };
}

test('accepts what capture writes and drops unknown fields', () => {
  const parsed = parseManifest({ ...valid, extra: true, error: 'stopped' });

  assert.equal(parsed.error, 'stopped');
  assert.deepEqual(parsed.results, valid.results);
  assert.deepEqual(parseManifest(withResult({ name: 'a', title: 'A', status: 'pending', x: 1 })).results, [
    { name: 'a', title: 'A', status: 'pending' },
  ]);
});

test('file names must follow from the scenario name', () => {
  for (const file of ['../startup.png', 'other.png', 'startup.png.exe', '/etc/passwd', 'startup.PNG']) {
    assert.throws(() => parseManifest(withResult({ name: 'startup', title: 'S', status: 'captured', file })), /file must be startup\.png/);
  }
  assert.throws(
    () => parseManifest(withResult({ name: 'a', title: 'A', status: 'failed', error: 'x', file: 'a.png' })),
    /file must be a\.failed\.png/,
  );
});

test('names must be safe path segments and unique', () => {
  for (const name of ['', '../x', 'a/b', 'A', 'a_b', 'a--b', '-a', 'a.png', 'a'.repeat(65)]) {
    assert.throws(() => parseManifest(withResult({ name, title: 'T', status: 'pending' })), /name/);
  }
  assert.throws(
    () => parseManifest({ results: [valid.results[2], valid.results[2]] }),
    /later appears twice/,
  );
});

test('titles cannot carry markup, mentions, links, or emoji codes', () => {
  const bad = ['@someone', 'see #12', '<img src=x>', '[x](y)', '![x](y)', '`x`', 'a|b', 'two\nlines', ' padded', 'https://example.com', ':tada:'];
  for (const text of bad) {
    assert.ok(textProblem(text, 300), text);
    assert.throws(() => parseManifest(withResult({ name: 'a', title: text, status: 'pending' })), /title/);
  }
  assert.equal(textProblem("needs the Code - OSS workbench (fork's build), 1/2: ready", 300), undefined);
  assert.ok(textProblem('x'.repeat(101), 100));
});

test('rejects wrong shapes and types', () => {
  for (const value of [null, [], 'x', {}, { results: {} }, { results: [1] }, { results: [], error: 3 }]) {
    assert.throws(() => parseManifest(value));
  }
  assert.throws(() => parseManifest(withResult({ name: 'a', title: 'A', status: 'done' })), /status/);
  assert.throws(() => parseManifest(withResult({ name: 'a', title: 'A', status: 'failed' })), /error/);
  assert.throws(() => parseManifest({ results: Array.from({ length: 51 }, (_, i) => ({ name: `s${String(i)}`, title: 'S', status: 'pending' })) }), /more than 50/);
});
