import assert from 'node:assert/strict';
import { test } from 'node:test';
import { code, fence, MARKER, renderComment, type CommentInput } from '../src/comment.ts';
import type { Manifest } from '../src/manifest.ts';

const headSha = 'abcdef1234567890abcdef1234567890abcdef12';
const base = 'https://raw.githubusercontent.com/o/r/0123456789abcdef0123456789abcdef01234567/pr-7/abcdef1';

function render(overrides: Partial<CommentInput>): string {
  return renderComment({
    serverUrl: 'https://github.com',
    repository: 'o/r',
    prNumber: 7,
    headSha,
    runUrl: 'https://github.com/o/r/actions/runs/1',
    manifest: undefined,
    images: { base, tree: 'https://github.com/o/r/tree/0123456/pr-7/abcdef1' },
    failedSteps: [],
    ...overrides,
  });
}

const mixed: Manifest = {
  results: [
    { name: 'startup', title: 'Startup', status: 'captured', file: 'startup.png' },
    { name: 'editor-file-open', title: 'Editor with a file open', status: 'not-available', reason: 'needs the editor' },
  ],
};

test('starts with the marker and embeds captured images by commit SHA', () => {
  const body = render({ manifest: mixed });

  assert.ok(body.startsWith(`${MARKER}\n## Screenshots`));
  assert.ok(body.includes(`![Startup](${base}/startup.png)`));
  assert.ok(body.includes('[`abcdef1`](https://github.com/o/r/pull/7/commits/' + headSha + ')'));
  assert.ok(body.includes(': 1 captured, 1 not available yet.'));
  assert.ok(body.includes('### Editor with a file open\n\nNot available yet: needs the editor.'));
  assert.ok(!body.includes('[!CAUTION]'));
});

test('a failed scenario shows its error and the screenshot taken when it failed', () => {
  const error = 'TimeoutError: locator.waitFor: Timeout 60000ms exceeded.\nCall log:\n  - waiting for `.x`';
  const body = render({
    manifest: {
      results: [
        ...mixed.results,
        { name: 'coordinator-chat', title: 'Coordinator chat', status: 'failed', error, file: 'coordinator-chat.failed.png' },
      ],
    },
  });

  assert.ok(body.includes('> [!CAUTION]\n> 1 of 3 scenarios failed.'));
  assert.ok(body.includes('Failed: `TimeoutError: locator.waitFor: Timeout 60000ms exceeded.`'));
  assert.ok(body.includes('```text\nTimeoutError: locator.waitFor'));
  assert.ok(body.includes(`![Coordinator chat when it failed](${base}/coordinator-chat.failed.png)`));
});

test('without a manifest it names the failed step and shows the end of its log', () => {
  const log = Array.from({ length: 100 }, (_, index) => `\u001b[31mline ${String(index)}\u001b[0m`).join('\n');
  const body = render({ images: undefined, failedSteps: [{ id: 'build', log }] });

  assert.ok(body.includes('> No screenshots: the `build` step failed.'));
  assert.ok(body.includes('line 99'));
  assert.ok(body.includes('line 60'));
  assert.ok(!body.includes('line 59\n'));
  assert.ok(!body.includes('\u001b'));
  assert.ok(!body.includes('###'));
});

test('capture that stops early reports the error and the scenarios that did not run', () => {
  const body = render({
    manifest: {
      error: 'scripts/ci/app-launch failed: app-launch: out/main.js does not exist',
      results: [{ name: 'startup', title: 'Startup', status: 'pending' }],
    },
  });

  assert.ok(body.includes('> Capture stopped early: `scripts/ci/app-launch failed: app-launch: out/main.js does not exist`'));
  assert.ok(body.includes('### Startup\n\nDid not run: capture stopped before this scenario.'));
});

test('a timed out capture step counts the scenarios that did not run', () => {
  const body = render({
    manifest: { results: [mixed.results[0] ?? assert.fail(), { name: 'x', title: 'X', status: 'pending' }] },
    failedSteps: [{ id: 'capture', log: 'Error: The operation was canceled.' }],
  });

  assert.ok(body.includes('> Capture stopped before 1 of 2 scenarios ran.'));
  assert.ok(body.includes("Last lines of the capture step's log"));
});

test('a failed push keeps the results and says the images are missing', () => {
  const body = render({ manifest: mixed, images: undefined, pushError: 'rejected' });

  assert.ok(body.includes('pushing them to the ci-screenshots branch failed'));
  assert.ok(body.includes('### Startup\n\nCaptured, but the image was not pushed.'));
});

test('rejected capture results are reported instead of rendered', () => {
  const body = render({
    manifest: undefined,
    images: undefined,
    artifactError: 'results[0].title must be one line\nof safe text',
    failedSteps: [{ id: 'capture', log: 'capture log' }],
  });

  assert.ok(body.includes("> No screenshots: the capture job's results were rejected: `results[0].title must be one line`."));
  assert.ok(body.includes('<details><summary>Why the results were rejected</summary>'));
  assert.ok(body.includes('capture log'));
  assert.ok(!body.includes('###'));
});

test('never mentions users or links issues, so it creates no notifications', () => {
  const body = render({
    manifest: {
      results: [{ name: 'a', title: 'A', status: 'failed', error: 'expected #12 and @someone' }],
    },
  });
  const outsideCode = body.replace(/```text\n[\s\S]*?\n```/g, '').replace(/`[^`\n]*`/g, '');

  assert.doesNotMatch(outsideCode, /(^|\s)#\d/);
  assert.doesNotMatch(outsideCode, /(^|\s)@\w/);
});

test('code spans and fences survive backticks in the text', () => {
  assert.equal(code('a `b` c'), '``a `b` c``');
  assert.equal(code('`x`'), '`` `x` ``');
  assert.equal(fence('```\nx\n```'), '````text\n```\nx\n```\n````');
});
