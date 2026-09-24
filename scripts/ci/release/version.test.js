'use strict';

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { mkdtempSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { after, describe, it } = require('node:test');
const {
  bumpLevel,
  compareVersions,
  computeVersion,
  latestTag,
  nextVersion,
  parseTag,
  parseVersion,
} = require('./version.js');

describe('parseVersion and compareVersions', () => {
  it('accepts only MAJOR.MINOR.PATCH without a v', () => {
    assert.deepEqual(parseVersion('0.10.2'), [0, 10, 2]);
    for (const version of ['v0.1.0', '0.1', '01.0.0', '0.1.0-rc.1', '0.1.0.1', '']) {
      assert.equal(parseVersion(version), null, version);
    }
  });

  it('orders versions by number', () => {
    const sorted = ['1.0.0', '0.10.0', '0.9.9', '0.9.10', '0.1.0'].map(parseVersion).sort(compareVersions);
    assert.deepEqual(sorted, [[0, 1, 0], [0, 9, 9], [0, 9, 10], [0, 10, 0], [1, 0, 0]]);
    assert.equal(compareVersions([1, 2, 3], [1, 2, 3]), 0);
  });
});

describe('parseTag and latestTag', () => {
  it('accepts only vMAJOR.MINOR.PATCH tags', () => {
    assert.deepEqual(parseTag('v1.20.3'), [1, 20, 3]);
    for (const tag of ['1.2.3', 'v1.2', 'v01.2.3', 'v1.2.3-rc.1', 'v1.2.3+build', 'nightly']) {
      assert.equal(parseTag(tag), null, tag);
    }
  });

  it('picks the highest release tag by number, not by string order', () => {
    assert.equal(latestTag(['v0.9.0', 'v0.10.0', 'v0.2.1', 'v1.0.0-rc.1', '']).tag, 'v0.10.0');
    assert.equal(latestTag(['', 'nightly']), null);
  });
});

describe('bumpLevel', () => {
  const cases = [
    ['feat: add coordinator chat (#12)', 'minor'],
    ['Feat(editor): add file tree', 'minor'],
    ['fix: handle missing config', 'patch'],
    ['chore: add ci workflow (#3)', 'patch'],
    ['docs: record naming decision', 'patch'],
    ['Merge pull request #40 from ryan-stoffel/develop', 'patch'],
    ['not a conventional commit', 'patch'],
    ['feat!: drop the v1 protocol', 'major'],
    ['fix(daemon)!: rename the socket', 'major'],
    ['refactor: move config\n\nBREAKING CHANGE: the config file moved', 'major'],
    ['chore: rename flag\n\nBREAKING-CHANGE: --foo is now --bar', 'major'],
    ['feat: add flag\n\nbreaking change: lowercase is not a footer', 'minor'],
    ['feat : a space before the colon is not a header', 'patch'],
  ];
  for (const [message, level] of cases) {
    it(`${JSON.stringify(message.split('\n')[0])} is ${level}`, () => {
      assert.equal(bumpLevel(message), level);
    });
  }
});

describe('nextVersion', () => {
  it('starts at 0.1.0 when there is no release yet, whatever the commits say', () => {
    assert.equal(nextVersion(null, []), '0.1.0');
    assert.equal(nextVersion(null, ['feat!: everything is new']), '0.1.0');
  });

  it('bumps below 1.0, where a breaking change is a minor bump', () => {
    assert.equal(nextVersion([0, 1, 0], ['fix: a', 'chore: b']), '0.1.1');
    assert.equal(nextVersion([0, 1, 4], ['fix: a', 'feat: b']), '0.2.0');
    assert.equal(nextVersion([0, 1, 4], ['feat!: c']), '0.2.0');
  });

  it('bumps from 1.0 on, where a breaking change is a major bump', () => {
    assert.equal(nextVersion([1, 2, 3], ['docs: a']), '1.2.4');
    assert.equal(nextVersion([1, 2, 3], ['feat: a', 'fix: b']), '1.3.0');
    assert.equal(nextVersion([1, 2, 3], ['fix: a', 'refactor: b\n\nBREAKING CHANGE: c']), '2.0.0');
  });

  it('refuses to release when nothing changed since the last tag', () => {
    assert.throws(() => nextVersion([0, 3, 0], []), /no commits since v0\.3\.0/);
  });
});

describe('computeVersion on a git repository', () => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'next-version-test-'));
  after(() => rmSync(scratch, { recursive: true, force: true }));

  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_AUTHOR_NAME: 'Test',
    GIT_AUTHOR_EMAIL: 'test@example.com',
    GIT_COMMITTER_NAME: 'Test',
    GIT_COMMITTER_EMAIL: 'test@example.com',
  };
  let count = 0;
  function repo() {
    count += 1;
    const dir = path.join(scratch, `repo-${count}`);
    execFileSync('git', ['init', '--quiet', '--initial-branch=main', dir], { env });
    return dir;
  }
  function git(dir, ...args) {
    return execFileSync('git', args, { cwd: dir, env, encoding: 'utf8' });
  }
  function commit(dir, message) {
    git(dir, 'commit', '--quiet', '--allow-empty', '-m', message);
  }
  function version(dir) {
    return computeVersion(dir, env).version;
  }

  it('is 0.1.0 with no tags', () => {
    const dir = repo();
    commit(dir, 'chore: initialize repository');
    commit(dir, 'feat!: start over');
    assert.equal(version(dir), '0.1.0');
  });

  it('bumps from the last release tag using the commits after it', () => {
    const dir = repo();
    commit(dir, 'chore: initialize repository');
    git(dir, 'tag', 'v0.1.0');
    commit(dir, 'fix: a');
    assert.equal(version(dir), '0.1.1');
    commit(dir, 'feat: b');
    assert.equal(version(dir), '0.2.0');
  });

  it('reuses the tag on HEAD, so re-running a release is safe', () => {
    const dir = repo();
    commit(dir, 'chore: initialize repository');
    git(dir, 'tag', 'v0.1.0');
    commit(dir, 'feat: b');
    git(dir, 'tag', 'v0.2.0');
    assert.equal(version(dir), '0.2.0');
  });

  it('follows develop merged into main with merge commits', () => {
    const dir = repo();
    commit(dir, 'chore: initialize repository');
    git(dir, 'switch', '--quiet', '-c', 'develop');
    commit(dir, 'feat: first feature (#10)');
    git(dir, 'switch', '--quiet', 'main');
    git(dir, 'merge', '--quiet', '--no-ff', '-m', 'Merge pull request #11 from owner/develop', 'develop');
    assert.equal(version(dir), '0.1.0');
    git(dir, 'tag', 'v0.1.0');
    git(dir, 'switch', '--quiet', 'develop');
    commit(dir, 'fix: second fix (#12)');
    commit(dir, 'docs: third change (#13)');
    git(dir, 'switch', '--quiet', 'main');
    git(dir, 'merge', '--quiet', '--no-ff', '-m', 'Merge pull request #14 from owner/develop', 'develop');
    assert.equal(version(dir), '0.1.1');
  });

  it('ignores release tags that are not merged into HEAD', () => {
    const dir = repo();
    commit(dir, 'chore: initialize repository');
    git(dir, 'tag', 'v0.1.0');
    git(dir, 'switch', '--quiet', '-c', 'side');
    commit(dir, 'feat: side work');
    git(dir, 'tag', 'v0.5.0');
    git(dir, 'switch', '--quiet', 'main');
    commit(dir, 'fix: main work');
    assert.equal(version(dir), '0.1.1');
  });

  it('refuses a shallow clone, which can hide older tags', () => {
    const source = repo();
    commit(source, 'chore: initialize repository');
    git(source, 'tag', 'v0.1.0');
    commit(source, 'fix: a');
    commit(source, 'fix: b');
    const clone = path.join(scratch, 'shallow');
    execFileSync('git', ['clone', '--quiet', '--depth', '1', `file://${source}`, clone], { env });
    assert.throws(() => version(clone), /shallow/);
  });
});
