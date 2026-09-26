import assert from 'node:assert/strict';
import { test } from 'node:test';
import { textProblem } from '../src/manifest.ts';
import {
  checkScenes,
  decide,
  formatRequest,
  parseBody,
  parseRequest,
  requestBlock,
  RequestError,
  type Pull,
  type PullEvent,
} from '../src/request.ts';
import { END, START } from '../src/section.ts';

const known = ['agents-window', 'agents-window-project', 'startup', 'editor-file-open'];
const block = '<!-- wisp-media\nafter: agents-window\nbefore-after: agents-window-project\nvideo: startup\n-->';

test('reads the example block from the pull request template', () => {
  const request = parseBody(`## Summary\n\nText.\n\n${block}\n\nCloses #1\n`);

  assert.deepEqual(request.entries, [
    { mode: 'after', name: 'agents-window' },
    { mode: 'before-after', name: 'agents-window-project' },
    { mode: 'video', name: 'startup' },
  ]);
  assert.equal(formatRequest(request), 'after: agents-window; before-after: agents-window-project; video: startup');
});

test('takes several scenes per line, comments, blank lines, CRLF, and its own one-line form', () => {
  const request = parseRequest('\r\n# the new views\r\nafter: startup,editor-file-open  agents-window\r\n\r\n');

  assert.deepEqual(
    request.entries.map((entry) => entry.name),
    ['startup', 'editor-file-open', 'agents-window'],
  );
  assert.deepEqual(parseRequest(formatRequest(request)), request);
});

test('a labeled pull request with no block fails with a clear message', () => {
  for (const body of [null, '', 'Closes #1', '<!-- wisp-media: after: startup -->', `${START}\n<!-- wisp-media\nafter: startup\n-->\n${END}`]) {
    assert.throws(() => parseBody(body), (error: unknown) => error instanceof RequestError && error.message.includes('no wisp-media block'));
  }
});

test('ignores a block inside the section the workflow writes, and reads the one outside it', () => {
  const body = `${START}\n<!-- wisp-media\nvideo: agents-window\n-->\n${END}\n\n<!-- wisp-media\nafter: startup\n-->`;

  assert.equal(requestBlock(body), 'after: startup\n');
});

test('rejects malformed lines, unknown modes, bad names, duplicates, and too many scenes', () => {
  const cases: [string, RegExp][] = [
    ['after agents-window', /Line 1 .* is not a mode, a colon, and scene names/],
    ['screenshot: startup', /Line 1 .* unknown mode \(screenshot\)\. Use after, before-after, video\./],
    ['After: startup', /Line 1 .* is not a mode/],
    ['after:', /Line 1 .* names no scene/],
    ['after: ../etc', /not lowercase letters and digits joined by hyphens/],
    ['after: Startup', /not lowercase letters/],
    ['after: startup\nvideo: startup', /names startup twice/],
    ['# only a comment', /names no scene/],
    [`after: ${Array.from({ length: 11 }, (_, index) => `s${String(index)}`).join(', ')}`, /11 scenes; the limit is 10/],
  ];
  for (const [text, message] of cases) {
    assert.throws(() => parseRequest(text), (error: unknown) => error instanceof RequestError && message.test(error.message), text);
  }
});

test('never echoes raw text from the body, only names that are safe to show', () => {
  for (const text of ['<img src=x>: startup', 'after: @someone', 'after: #12', 'x: `code`', 'after: https://example.com']) {
    assert.throws(
      () => parseRequest(text),
      (error: unknown) => error instanceof Error && !/[<@#`]|:\/\//.test(error.message),
      text,
    );
  }
});

test('names every unknown scene and lists the known ones', () => {
  assert.throws(
    () => {
      checkScenes(parseRequest('after: startup, nope\nvideo: gone'), known);
    },
    /Unknown scenes: nope, gone\. Scenes are the names in ci\/screenshots\/src\/scenarios\.ts: agents-window, agents-window-project, startup, editor-file-open\./,
  );
  assert.doesNotThrow(() => {
    checkScenes(parseRequest('after: startup'), known);
  });
});

test('every problem is text that publish accepts', () => {
  const problems = ['after agents-window', 'screenshot: startup', 'after: A', 'after: a\nafter: a', 'after: nope'].map((text) => {
    try {
      checkScenes(parseRequest(text), known);
    } catch (error) {
      return (error as Error).message;
    }
    return assert.fail(text);
  });
  try {
    parseBody('');
  } catch (error) {
    problems.push((error as Error).message);
  }
  for (const problem of problems) {
    assert.equal(textProblem(problem, 2_000), undefined, problem);
  }
});

const repository = 'o/r';
const pull: Pull = { state: 'open', headRepo: repository, labels: ['type:feature', 'screenshots'], body: `Intro\n\n${block}` };
const synchronize: PullEvent = { name: 'pull_request', action: 'synchronize' };

test('runs a labeled pull request with a valid block', () => {
  const decision = decide(synchronize, pull, repository, known);

  assert.ok(decision.kind === 'run');
  assert.equal(formatRequest(decision.request), 'after: agents-window; before-after: agents-window-project; video: startup');
  assert.equal(decide({ name: 'workflow_dispatch' }, pull, repository, known).kind, 'run');
  assert.equal(decide({ name: 'pull_request', action: 'labeled', label: 'screenshots' }, pull, repository, known).kind, 'run');
});

test('never captures for an unlabeled pull request', () => {
  const unlabeled = { ...pull, labels: ['type:feature'] };

  assert.equal(decide(synchronize, unlabeled, repository, known).kind, 'skip');
  assert.deepEqual(decide({ name: 'workflow_dispatch' }, unlabeled, repository, known), {
    kind: 'problem',
    problem: 'The pull request does not have the screenshots label, and nothing is captured without it.',
    publish: false,
  });
});

test('skips forks, other labels, and edits that leave the block and base alone', () => {
  assert.equal(decide(synchronize, { ...pull, headRepo: 'someone/fork' }, repository, known).kind, 'skip');
  assert.equal(decide(synchronize, { ...pull, headRepo: undefined }, repository, known).kind, 'skip');
  assert.equal(decide({ name: 'pull_request', action: 'labeled', label: 'priority:high' }, pull, repository, known).kind, 'skip');

  const edited = (changes: PullEvent['changes']): PullEvent => ({ name: 'pull_request', action: 'edited', changes });
  assert.equal(decide(edited({ body: { from: `Old intro\n\n${block}` } }), pull, repository, known).kind, 'skip');
  assert.equal(decide(edited({}), pull, repository, known).kind, 'skip');
  assert.equal(decide(edited({ body: { from: 'Old intro' } }), pull, repository, known).kind, 'run');
  assert.equal(decide(edited({ base: { ref: { from: 'main' } } }), pull, repository, known).kind, 'run');
});

test('reports a bad request in the section, but a closed pull request only in the log', () => {
  assert.deepEqual(decide(synchronize, { ...pull, body: 'no block' }, repository, known).kind, 'problem');
  const unknown = decide(synchronize, { ...pull, body: '<!-- wisp-media\nafter: nope\n-->' }, repository, known);
  assert.equal(unknown.kind === 'problem' && unknown.publish, true);
  const closed = decide(synchronize, { ...pull, state: 'closed' }, repository, known);
  assert.equal(closed.kind === 'problem' && closed.publish, false);
});
