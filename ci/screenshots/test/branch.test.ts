import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { commitFiles, type CommitFilesOptions } from '../src/branch.ts';

const branch = 'ci-screenshots';
const identity = {
  GIT_AUTHOR_NAME: 'test',
  GIT_AUTHOR_EMAIL: 'test@example.com',
  GIT_COMMITTER_NAME: 'test',
  GIT_COMMITTER_EMAIL: 'test@example.com',
};

let root = '';

before(async () => {
  root = await mkdtemp(join(tmpdir(), 'wisp-branch-test-'));
});

after(async () => {
  await rm(root, { recursive: true, force: true });
});

interface Remote {
  dir: string;
  url: string;
}

function bareRemote(name: string): Remote {
  const dir = join(root, `${name}.git`);
  execFileSync('git', ['init', '--quiet', '--bare', dir]);
  execFileSync('git', ['-C', dir, 'config', 'uploadpack.allowFilter', 'true']);
  return { dir, url: `file://${dir}` };
}

async function publish(
  remote: Remote,
  pr: number,
  content: string,
  extra: Partial<CommitFilesOptions> = {},
): Promise<string> {
  const source = join(root, `source-${String(pr)}-${String(Math.random()).slice(2)}`);
  await mkdir(source);
  await writeFile(join(source, 'startup.png'), content);
  return commitFiles({
    remote: remote.url,
    branch,
    files: [{ path: `pr-${String(pr)}/abc1234/startup.png`, source: join(source, 'startup.png') }],
    message: `test: pr-${String(pr)}`,
    env: { ...process.env, ...identity },
    retryDelayMs: 5,
    log: () => undefined,
    ...extra,
  });
}

function git(remote: Remote, ...args: string[]): string {
  return execFileSync('git', ['-C', remote.dir, ...args], { encoding: 'utf8' }).trim();
}

function tip(remote: Remote): string {
  return git(remote, 'rev-parse', `refs/heads/${branch}`);
}

function paths(remote: Remote): string[] {
  return git(remote, 'ls-tree', '-r', '--name-only', `refs/heads/${branch}`).split('\n');
}

function parents(remote: Remote, commit: string): string[] {
  return git(remote, 'rev-list', '--parents', '-n', '1', commit).split(' ').slice(1);
}

test('creates the branch as a root commit when it is missing', async () => {
  const remote = bareRemote('create');

  const commit = await publish(remote, 1, 'one');

  assert.equal(tip(remote), commit);
  assert.deepEqual(parents(remote, commit), []);
  assert.deepEqual(paths(remote), ['pr-1/abc1234/startup.png']);
  assert.equal(git(remote, 'show', `${commit}:pr-1/abc1234/startup.png`), 'one');
});

test('adds files on top of the current tip', async () => {
  const remote = bareRemote('stack');
  const first = await publish(remote, 1, 'one');

  const second = await publish(remote, 2, 'two');

  assert.deepEqual(parents(remote, second), [first]);
  assert.deepEqual(paths(remote), ['pr-1/abc1234/startup.png', 'pr-2/abc1234/startup.png']);
});

test('makes no commit when the tip already has the same files', async () => {
  const remote = bareRemote('noop');
  const first = await publish(remote, 1, 'one');

  const again = await publish(remote, 1, 'one');

  assert.equal(again, first);
  assert.equal(git(remote, 'rev-list', '--count', `refs/heads/${branch}`), '1');
});

test('replays its commit on the new tip when another push lands first', async () => {
  const remote = bareRemote('race');
  await publish(remote, 1, 'one');
  let competitor = '';
  const attempts: number[] = [];

  const commit = await publish(remote, 2, 'two', {
    beforePush: async (attempt) => {
      attempts.push(attempt);
      if (attempt === 1) {
        competitor = await publish(remote, 3, 'three');
      }
    },
  });

  assert.deepEqual(attempts, [1, 2]);
  assert.deepEqual(parents(remote, commit), [competitor]);
  assert.deepEqual(paths(remote), [
    'pr-1/abc1234/startup.png',
    'pr-2/abc1234/startup.png',
    'pr-3/abc1234/startup.png',
  ]);
});

test('two runs that both find the branch missing both land', async () => {
  const remote = bareRemote('create-race');
  let competitor = '';

  const commit = await publish(remote, 1, 'one', {
    beforePush: async (attempt) => {
      if (attempt === 1) {
        competitor = await publish(remote, 2, 'two');
      }
    },
  });

  assert.deepEqual(parents(remote, competitor), []);
  assert.deepEqual(parents(remote, commit), [competitor]);
  assert.deepEqual(paths(remote), ['pr-1/abc1234/startup.png', 'pr-2/abc1234/startup.png']);
});

test('concurrent runs all land without force pushes', async () => {
  const remote = bareRemote('concurrent');
  await publish(remote, 1, 'one');

  const commits = await Promise.all([2, 3, 4, 5].map((pr) => publish(remote, pr, `content ${String(pr)}`)));

  assert.equal(git(remote, 'rev-list', '--count', `refs/heads/${branch}`), '5');
  for (const commit of commits) {
    execFileSync('git', ['-C', remote.dir, 'merge-base', '--is-ancestor', commit, `refs/heads/${branch}`]);
  }
  assert.equal(paths(remote).length, 5);
});

test('fails after the last attempt', async () => {
  const missing: Remote = { dir: join(root, 'missing.git'), url: `file://${join(root, 'missing.git')}` };

  await assert.rejects(publish(missing, 1, 'one', { attempts: 2 }));
});
