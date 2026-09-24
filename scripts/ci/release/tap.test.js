'use strict';

const assert = require('node:assert/strict');
const { describe, it } = require('node:test');
const { renderCask } = require('./cask.js');
const { planTapCommit } = require('./tap.js');

const cask = (version, sha = 'a') => renderCask(version, { arm64: sha.repeat(64) });

describe('planTapCommit', () => {
  it('adds the cask when the tap has none', () => {
    assert.deepEqual(planTapCommit(null, cask('0.1.0'), '0.1.0'), {
      action: 'commit',
      headline: 'wisp 0.1.0 (new cask)',
    });
  });

  it('skips an identical cask, so re-running a release commits nothing', () => {
    assert.deepEqual(planTapCommit(cask('0.1.0'), cask('0.1.0'), '0.1.0'), { action: 'skip' });
  });

  it('upgrades an older cask', () => {
    assert.deepEqual(planTapCommit(cask('0.1.0'), cask('0.2.0'), '0.2.0'), {
      action: 'commit',
      headline: 'wisp 0.2.0',
    });
  });

  it('refuses to replace a newer cask, so an old run cannot downgrade users', () => {
    const plan = planTapCommit(cask('0.2.0'), cask('0.1.0'), '0.1.0');
    assert.equal(plan.action, 'refuse');
    assert.match(plan.reason, /already has wisp 0\.2\.0, newer than 0\.1\.0; not downgrading/);
  });

  it('compares versions as numbers, not strings', () => {
    assert.equal(planTapCommit(cask('0.10.0'), cask('0.9.0'), '0.9.0').action, 'refuse');
    assert.equal(planTapCommit(cask('0.9.0'), cask('0.10.0'), '0.10.0').action, 'commit');
  });

  it('replaces a different cask for the same version, which re-syncs it with the release', () => {
    assert.deepEqual(planTapCommit(cask('0.1.0', 'a'), cask('0.1.0', 'b'), '0.1.0'), {
      action: 'commit',
      headline: 'wisp 0.1.0',
    });
  });

  it('leaves a cask whose version it cannot read alone', () => {
    const plan = planTapCommit('cask "wisp" do\n  version :latest\nend\n', cask('0.1.0'), '0.1.0');
    assert.equal(plan.action, 'refuse');
    assert.match(plan.reason, /not MAJOR\.MINOR\.PATCH/);
  });

  it('rejects a version that is not MAJOR.MINOR.PATCH', () => {
    assert.throws(() => planTapCommit(null, cask('0.1.0'), 'v0.1.0'), /MAJOR\.MINOR\.PATCH/);
  });
});
