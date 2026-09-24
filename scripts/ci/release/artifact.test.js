'use strict';

const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { after, describe, it } = require('node:test');
const { checkArtifact } = require('./artifact.js');
const { caskFromZips } = require('./cask.js');

describe('checkArtifact', () => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'check-release-artifact-test-'));
  after(() => rmSync(scratch, { recursive: true, force: true }));
  let count = 0;

  function artifact(files, { cask = true } = {}) {
    count += 1;
    const dir = path.join(scratch, `artifact-${count}`);
    mkdirSync(dir);
    for (const [name, content] of Object.entries(files)) {
      writeFileSync(path.join(dir, name), content);
    }
    if (cask) {
      const zips = Object.keys(files).map((name) => path.join(dir, name));
      writeFileSync(path.join(dir, 'wisp.rb'), caskFromZips('0.1.0', zips));
    }
    return dir;
  }

  it('accepts exactly the expected zips with a matching cask, and lists their sha256', () => {
    const dir = artifact({ 'wisp-0.1.0-arm64.zip': 'arm', 'wisp-0.1.0-x64.zip': 'intel' });
    const sha = createHash('sha256').update('arm').digest('hex');
    assert.deepEqual(checkArtifact(dir, '0.1.0', ['arm64', 'x64'])[0], `${sha}  wisp-0.1.0-arm64.zip`);
  });

  it('rejects a cask whose sha256 does not match the zip', () => {
    const dir = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' });
    writeFileSync(path.join(dir, 'wisp-0.1.0-arm64.zip'), 'tampered');
    assert.throws(() => checkArtifact(dir, '0.1.0', ['arm64']), /does not match the cask generated/);
  });

  it('rejects missing, extra, or misnamed files', () => {
    const arm = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' });
    assert.throws(() => checkArtifact(arm, '0.1.0', ['arm64', 'x64']), /expected exactly/);
    assert.throws(() => checkArtifact(arm, '0.2.0', ['arm64']), /expected exactly/);
    const extra = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' });
    writeFileSync(path.join(extra, 'notes.txt'), 'x');
    assert.throws(() => checkArtifact(extra, '0.1.0', ['arm64']), /expected exactly/);
    const noCask = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' }, { cask: false });
    assert.throws(() => checkArtifact(noCask, '0.1.0', ['arm64']), /expected exactly/);
  });

  it('rejects a symlink in place of a zip', () => {
    const dir = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' });
    const link = path.join(dir, 'wisp-0.1.0-x64.zip');
    symlinkSync(path.join(dir, 'wisp-0.1.0-arm64.zip'), link);
    assert.throws(() => checkArtifact(dir, '0.1.0', ['arm64', 'x64']), /not a regular file/);
  });

  it('needs at least one arch', () => {
    const dir = artifact({ 'wisp-0.1.0-arm64.zip': 'arm' });
    assert.throws(() => checkArtifact(dir, '0.1.0', []), /at least one arch/);
  });
});
