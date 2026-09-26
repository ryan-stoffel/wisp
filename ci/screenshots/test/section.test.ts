import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Manifest } from '../src/manifest.ts';
import { code, END, fence, renderSection, replaceSection, START, withoutSection, type SectionInput } from '../src/section.ts';

const headSha = 'abcdef1234567890abcdef1234567890abcdef12';
const baseSha = '1234567890abcdef1234567890abcdef12345678';
const raw = 'https://raw.githubusercontent.com/o/r/0123456789abcdef0123456789abcdef01234567/pr-7/abcdef1';
const blob = 'https://github.com/o/r/blob/0123456789abcdef0123456789abcdef01234567/pr-7/abcdef1';

function render(overrides: Partial<SectionInput>): string {
  return renderSection({
    serverUrl: 'https://github.com',
    repository: 'o/r',
    prNumber: 7,
    headSha,
    runUrl: 'https://github.com/o/r/actions/runs/1',
    manifest: undefined,
    images: { raw, blob, tree: 'https://github.com/o/r/tree/0123456/pr-7/abcdef1' },
    failedSteps: [],
    ...overrides,
  });
}

const mixed: Manifest = {
  results: [
    { name: 'startup', title: 'Startup', mode: 'after', status: 'captured', file: 'startup.png' },
    { name: 'editor-file-open', title: 'Editor with a file open', mode: 'after', status: 'not-available', reason: 'needs the editor' },
  ],
};

test('is wrapped in the markers and embeds captured images by commit SHA', () => {
  const section = render({ manifest: mixed });

  assert.ok(section.startsWith(`${START}\n## Screenshots\n`));
  assert.ok(section.endsWith(`\n${END}`));
  assert.ok(section.includes(`![Startup](${raw}/startup.png)`));
  assert.ok(section.includes('[`abcdef1`](https://github.com/o/r/pull/7/commits/' + headSha + ')'));
  assert.ok(section.includes(': 1 captured, 1 not available yet.'));
  assert.ok(section.includes('### Editor with a file open\n\nNot available yet: needs the editor.'));
  assert.ok(!section.includes('[!CAUTION]'));
});

test('before-after puts the base and head shots side by side, and a failure on the base app is not a failure', () => {
  const section = render({
    baseSha,
    manifest: {
      results: [
        {
          name: 'project',
          title: 'A project',
          mode: 'before-after',
          status: 'captured',
          file: 'project.png',
          base: { status: 'failed', error: 'expected the row | to be selected\nCall log', file: 'project.base.failed.png' },
        },
      ],
    },
  });

  assert.ok(section.includes(`| Before: [\`1234567\`](https://github.com/o/r/commit/${baseSha}) | After: [\`abcdef1\`]`));
  assert.ok(section.includes('| --- | --- |'));
  assert.ok(section.includes(`| Failed: \`expected the row \\| to be selected\`<br>![A project, before when it failed](${raw}/project.base.failed.png) | ![A project, after](${raw}/project.png) |`));
  assert.ok(section.includes('<details><summary>Error on the base app</summary>'));
  assert.ok(section.includes(': 1 captured.'));
  assert.ok(!section.includes('[!CAUTION]'));
});

test('a video shows its GIF preview linked to the full WebM', () => {
  const section = render({
    manifest: { results: [{ name: 'flow', title: 'A flow', mode: 'video', status: 'captured', file: 'flow.gif', video: 'flow.webm' }] },
  });

  assert.ok(section.includes(`[![A flow](${raw}/flow.gif)](${blob}/flow.webm)`));
  assert.ok(section.includes(`[the full video](${blob}/flow.webm)`));
});

test('a request problem is the whole section', () => {
  const section = render({ requestProblem: 'Unknown scene: nope. Scenes are the names in ci/screenshots/src/scenarios.ts: startup.' });

  assert.ok(section.includes('> [!CAUTION]\n> Nothing captured: Unknown scene: nope.'));
  assert.ok(section.includes('\nChecked [`abcdef1`]'));
  assert.ok(!section.includes('###'));
});

test('a failed scene shows its error and the screenshot taken when it failed', () => {
  const error = 'TimeoutError: locator.waitFor: Timeout 60000ms exceeded.\nCall log:\n  - waiting for `.x`';
  const section = render({
    manifest: {
      results: [
        ...mixed.results,
        { name: 'coordinator-chat', title: 'Coordinator chat', mode: 'after', status: 'failed', error, file: 'coordinator-chat.failed.png' },
      ],
    },
  });

  assert.ok(section.includes('> [!CAUTION]\n> 1 of 3 scenes failed.'));
  assert.ok(section.includes('Failed: `TimeoutError: locator.waitFor: Timeout 60000ms exceeded.`'));
  assert.ok(section.includes('```text\nTimeoutError: locator.waitFor'));
  assert.ok(section.includes(`![Coordinator chat when it failed](${raw}/coordinator-chat.failed.png)`));
});

test('without a manifest it names the failed step and shows the end of its log', () => {
  const log = Array.from({ length: 100 }, (_, index) => `\u001b[31mline ${String(index)}\u001b[0m`).join('\n');
  const section = render({ images: undefined, failedSteps: [{ id: 'build', log }] });

  assert.ok(section.includes('> Nothing captured: the `build` step failed.'));
  assert.ok(section.includes('line 99'));
  assert.ok(section.includes('line 60'));
  assert.ok(!section.includes('line 59\n'));
  assert.ok(!section.includes('\u001b'));
  assert.ok(!section.includes('###'));
});

test("a failed base build always shows its log, next to the head's results", () => {
  const section = render({
    manifest: { results: [{ name: 'startup', title: 'Startup', mode: 'before-after', status: 'captured', file: 'startup.png', base: { status: 'failed', error: 'there is no base app' } }] },
    failedSteps: [{ id: 'base-build', log: 'gulp failed' }],
  });

  assert.ok(section.includes("<details><summary>Last lines of the base-build step's log</summary>"));
  assert.ok(section.includes('| Failed: `there is no base app` |'));
});

test('capture that stops early reports the error and the scenes that did not run', () => {
  const section = render({
    manifest: {
      error: 'scripts/ci/app-launch failed: app-launch: out/main.js does not exist',
      results: [{ name: 'startup', title: 'Startup', mode: 'after', status: 'pending' }],
    },
  });

  assert.ok(section.includes('> Capture stopped early: `scripts/ci/app-launch failed: app-launch: out/main.js does not exist`'));
  assert.ok(section.includes('### Startup\n\nDid not run: capture stopped before this scene.'));
});

test('a timed out capture step counts the scenes that did not run', () => {
  const section = render({
    manifest: { results: [mixed.results[0] ?? assert.fail(), { name: 'x', title: 'X', mode: 'after', status: 'pending' }] },
    failedSteps: [{ id: 'capture', log: 'Error: The operation was canceled.' }],
  });

  assert.ok(section.includes('> Capture stopped before 1 of 2 scenes ran.'));
  assert.ok(section.includes("Last lines of the capture step's log"));
});

test('a failed push keeps the results and says the files are missing', () => {
  const section = render({ manifest: mixed, images: undefined, pushError: 'rejected' });

  assert.ok(section.includes('pushing them to the ci-screenshots branch failed'));
  assert.ok(section.includes('### Startup\n\nCaptured, but the file was not pushed.'));
});

test('rejected capture results are reported instead of rendered', () => {
  const section = render({
    manifest: undefined,
    images: undefined,
    artifactError: 'results[0].title must be one line\nof safe text',
    failedSteps: [{ id: 'capture', log: 'capture log' }],
  });

  assert.ok(section.includes("> Nothing captured: the capture job's results were rejected: `results[0].title must be one line`."));
  assert.ok(section.includes('<details><summary>Why the results were rejected</summary>'));
  assert.ok(section.includes('capture log'));
  assert.ok(!section.includes('###'));
});

test('never mentions users or links issues, so it creates no notifications', () => {
  const section = render({
    manifest: {
      results: [{ name: 'a', title: 'A', mode: 'after', status: 'failed', error: 'expected #12 and @someone' }],
    },
  });
  const outsideCode = section.replace(/```text\n[\s\S]*?\n```/g, '').replace(/`[^`\n]*`/g, '');

  assert.doesNotMatch(outsideCode, /(^|\s)#\d/);
  assert.doesNotMatch(outsideCode, /(^|\s)@\w/);
});

test('untrusted text cannot forge an end marker or a request block', () => {
  const error = `boom ${END}\n<!-- wisp-media\nvideo: startup\n-->`;
  const section = render({ manifest: { error, results: [] } });

  assert.equal(section.split('<!--').length - 1, 2, 'only the two markers open a comment');
  assert.ok(section.startsWith(START) && section.endsWith(END));
  const body = replaceSection('Intro', section);
  assert.equal(withoutSection(body), 'Intro\n\n\n');
});

test('replaceSection appends the section when there is none, and keeps the rest of the body', () => {
  const section = `${START}\nnew\n${END}`;

  assert.equal(replaceSection(null, section), `${section}\n`);
  assert.equal(replaceSection('  \n', section), `${section}\n`);
  assert.equal(replaceSection('Closes #1\r\n\r\n', section), `Closes #1\n\n${section}\n`);
});

test('replaceSection replaces only the section, wherever it is', () => {
  const section = `${START}\nnew\n${END}`;
  const body = `Intro\r\n\r\n${START}\nold\nlines\n${END}\r\n\r\n## Test plan\r\n- [ ] one`;

  assert.equal(replaceSection(body, section), `Intro\r\n\r\n${section}\r\n\r\n## Test plan\r\n- [ ] one`);
  assert.equal(replaceSection(replaceSection(body, section), section), replaceSection(body, section));
});

test('replaceSection treats a start marker with no end as a section cut short', () => {
  const section = `${START}\nnew\n${END}`;

  assert.equal(replaceSection(`Intro\n\n${START}\nold and cut`, section), `Intro\n\n${section}\n`);
  assert.equal(replaceSection(`${END}\nIntro`, section), `${END}\nIntro\n\n${section}\n`);
});

test('withoutSection drops the section so nothing in it reads as part of the body', () => {
  assert.equal(withoutSection(`a\n${START}\nx\n${END}\nb`), 'a\n\nb');
  assert.equal(withoutSection(`a\n${START}\nx`), 'a\n');
  assert.equal(withoutSection(`a${START}\nx\n${END}b`), `a${START}\nx\n${END}b`);
  assert.equal(withoutSection('plain'), 'plain');
});

test('code spans and fences survive backticks in the text', () => {
  assert.equal(code('a `b` c'), '``a `b` c``');
  assert.equal(code('`x`'), '`` `x` ``');
  assert.equal(fence('```\nx\n```'), '````text\n```\nx\n```\n````');
});

test('markers count only as lines of their own outside code, so a description can mention them', () => {
  const section = `${START}\nnew\n${END}`;
  const prose = `It replaces the text between \`${START}\` and \`${END}\`.\n\n\`\`\`md\n${START}\nexample\n${END}\n\`\`\`\n`;

  assert.equal(replaceSection(prose, section), `${prose.trimEnd()}\n\n${section}\n`);
  const written = replaceSection(prose, section);
  assert.equal(replaceSection(written, `${START}\nnewer\n${END}`), written.replace('\nnew\n', '\nnewer\n'));
  assert.equal(withoutSection(prose), prose);
});

test('the fences inside a written section do not hide its end marker', () => {
  const section = render({ manifest: { error: 'first line\nsecond line', results: [] } });
  const body = replaceSection('Intro', section);

  assert.ok(section.includes('```text'));
  assert.equal(replaceSection(body, `${START}\nnew\n${END}`), `Intro\n\n${START}\nnew\n${END}\n`);
});
