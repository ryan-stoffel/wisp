'use strict';

const { execFileSync } = require('node:child_process');

const FIRST_VERSION = '0.1.0';
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const HEADER = /^(\w+)(?:\([^()\r\n]*\))?(!)?: \S/;
const BREAKING_FOOTER = /^BREAKING[ -]CHANGE: /m;
const LEVELS = ['patch', 'minor', 'major'];
const CHANGE_NAMES = { patch: 'fix or other', minor: 'feat', major: 'breaking' };

function parseVersion(version) {
  const match = VERSION.exec(version);
  return match ? match.slice(1, 4).map(Number) : null;
}

function parseTag(tag) {
  return tag.startsWith('v') ? parseVersion(tag.slice(1)) : null;
}

function compareVersions(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) {
      return a[i] - b[i];
    }
  }
  return 0;
}

function latestTag(tags) {
  let latest = null;
  for (const tag of tags) {
    const version = parseTag(tag.trim());
    if (version && (!latest || compareVersions(version, latest.version) > 0)) {
      latest = { tag: tag.trim(), version };
    }
  }
  return latest;
}

function bumpLevel(message) {
  const header = HEADER.exec(message.split('\n', 1)[0]);
  if ((header && header[2] === '!') || BREAKING_FOOTER.test(message)) {
    return 'major';
  }
  return header && header[1].toLowerCase() === 'feat' ? 'minor' : 'patch';
}

function highestLevel(messages) {
  return LEVELS[Math.max(...messages.map((message) => LEVELS.indexOf(bumpLevel(message))))];
}

function nextVersion(current, messages) {
  if (!current) {
    return FIRST_VERSION;
  }
  if (messages.length === 0) {
    throw new Error(`no commits since v${current.join('.')}, so there is nothing to release`);
  }
  const level = highestLevel(messages);
  const [major, minor, patch] = current;
  if (level === 'major' && major > 0) {
    return `${major + 1}.0.0`;
  }
  if (level === 'patch') {
    return `${major}.${minor}.${patch + 1}`;
  }
  return `${major}.${minor + 1}.0`;
}

function computeVersion(repoDir, env = process.env) {
  const git = (...args) => execFileSync('git', args, { cwd: repoDir, env, encoding: 'utf8' });
  if (git('rev-parse', '--is-shallow-repository').trim() === 'true') {
    throw new Error('the checkout is shallow, so older release tags may be missing; fetch full history (fetch-depth: 0)');
  }
  const onHead = latestTag(git('tag', '--points-at', 'HEAD').split('\n'));
  if (onHead) {
    return { version: onHead.version.join('.'), reason: `HEAD is already tagged ${onHead.tag}` };
  }
  const latest = latestTag(git('tag', '--merged', 'HEAD').split('\n'));
  if (!latest) {
    return { version: FIRST_VERSION, reason: 'no release tag yet, so this is the first release' };
  }
  const messages = git('log', '-z', '--no-show-signature', '--format=%B', `${latest.tag}..HEAD`)
    .split('\0')
    .map((message) => message.trim())
    .filter(Boolean);
  const version = nextVersion(latest.version, messages);
  return {
    version,
    reason: `${messages.length} commit(s) since ${latest.tag}, largest change: ${CHANGE_NAMES[highestLevel(messages)]}`,
  };
}

module.exports = {
  bumpLevel,
  compareVersions,
  computeVersion,
  latestTag,
  nextVersion,
  parseTag,
  parseVersion,
};
