import assert from 'node:assert/strict';
import { test } from 'node:test';
import { nameProblem, parseManifest, textProblem } from '../src/manifest.ts';

const valid = {
  results: [
    { name: 'startup', title: 'Startup', mode: 'after', status: 'captured', file: 'startup.png' },
    { name: 'editor-file-open', title: 'Editor with a file open', mode: 'after', status: 'not-available', reason: 'needs the editor' },
    { name: 'chat', title: 'Chat', mode: 'after', status: 'failed', error: 'Timeout #12 @someone <b>', file: 'chat.failed.png' },
    { name: 'later', title: 'Later', mode: 'after', status: 'pending' },
  ],
};

function withResult(result: Record<string, unknown>): unknown {
  return { results: [{ mode: 'after', ...result }] };
}

test('accepts what capture writes and drops unknown fields', () => {
  const parsed = parseManifest({ ...valid, extra: true, error: 'stopped' });

  assert.equal(parsed.error, 'stopped');
  assert.deepEqual(parsed.results, valid.results);
  assert.deepEqual(parseManifest(withResult({ name: 'a', title: 'A', status: 'pending', x: 1 })).results, [
    { name: 'a', title: 'A', mode: 'after', status: 'pending' },
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
    () => parseManifest({ results: [valid.results[3], valid.results[3]] }),
    /later appears twice/,
  );
});

test('titles and reasons cannot carry markup, mentions, links, or emoji codes', () => {
  const bad = ['@someone', 'see #12', '<img src=x>', '[x](y)', '![x](y)', '`x`', 'a|b', 'two\nlines', ' padded', 'https://example.com', ':tada:'];
  for (const text of bad) {
    assert.ok(textProblem(text, 300), text);
    assert.throws(() => parseManifest(withResult({ name: 'a', title: text, status: 'pending' })), /title/);
    assert.throws(
      () => parseManifest(withResult({ name: 'a', title: 'A', status: 'not-available', reason: text })),
      /reason/,
    );
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
  assert.throws(() => parseManifest({ results: Array.from({ length: 51 }, (_, i) => ({ name: `s${String(i)}`, title: 'S', mode: 'after', status: 'pending' })) }), /more than 50/);
});

test('names that capture uses are accepted', () => {
  for (const name of ['startup', 'editor-file-open', 'coordinator-chat', 'a1-b2']) {
    assert.equal(nameProblem(name), undefined);
  }
});

test('before-after needs a base shot with its own file names', () => {
  const entry = { name: 'p', title: 'P', mode: 'before-after', status: 'captured', file: 'p.png' };

  assert.deepEqual(parseManifest(withResult({ ...entry, base: { status: 'captured', file: 'p.base.png' } })).results[0], {
    ...entry,
    base: { status: 'captured', file: 'p.base.png' },
  });
  assert.throws(() => parseManifest(withResult(entry)), /base is not an object/);
  assert.throws(() => parseManifest(withResult({ ...entry, base: { status: 'captured', file: 'p.png' } })), /base\.file must be p\.base\.png/);
  assert.throws(
    () => parseManifest(withResult({ ...entry, base: { status: 'failed', error: 'x', file: 'p.failed.png' } })),
    /base\.file must be p\.base\.failed\.png/,
  );
  assert.throws(() => parseManifest(withResult({ ...entry, mode: 'after', base: { status: 'pending' } })), /base is only for before-after/);
});

test('a captured video is a GIF with its WebM, and only a video has one', () => {
  const entry = { name: 'v', title: 'V', mode: 'video', status: 'captured', file: 'v.gif', video: 'v.webm' };

  assert.deepEqual(parseManifest(withResult(entry)).results[0], entry);
  assert.throws(() => parseManifest(withResult({ ...entry, file: 'v.png' })), /file must be v\.gif/);
  assert.throws(() => parseManifest(withResult({ ...entry, video: '../v.webm' })), /video must be v\.webm/);
  assert.throws(() => parseManifest(withResult({ ...entry, video: undefined })), /video must be v\.webm/);
  assert.throws(() => parseManifest(withResult({ ...entry, mode: 'after', file: 'v.png' })), /video is only for a captured video/);
  assert.throws(() => parseManifest(withResult({ ...entry, mode: 'slides' })), /mode is not after, before-after, video/);
});
