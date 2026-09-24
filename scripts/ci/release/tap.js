'use strict';

const { compareVersions, parseVersion } = require('./version.js');

const VERSION_LINE = /^\s*version\s+"([^"]*)"/m;

function planTapCommit(current, cask, version) {
  const ours = parseVersion(version);
  if (!ours) {
    throw new Error(`version must be MAJOR.MINOR.PATCH, got '${version}'`);
  }
  if (current === null) {
    return { action: 'commit', headline: `wisp ${version} (new cask)` };
  }
  if (current === cask) {
    return { action: 'skip' };
  }
  const found = VERSION_LINE.exec(current);
  const theirs = found ? parseVersion(found[1]) : null;
  if (!theirs) {
    return { action: 'refuse', reason: 'its version is not MAJOR.MINOR.PATCH, so it is left for a person to fix' };
  }
  if (compareVersions(theirs, ours) > 0) {
    return { action: 'refuse', reason: `it already has wisp ${found[1]}, newer than ${version}; not downgrading` };
  }
  return { action: 'commit', headline: `wisp ${version}` };
}

module.exports = { planTapCommit };
