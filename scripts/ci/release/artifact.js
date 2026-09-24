'use strict';

const { createHash } = require('node:crypto');
const { lstatSync, readFileSync, readdirSync } = require('node:fs');
const path = require('node:path');
const { caskFromZips } = require('./cask.js');

function checkArtifact(dir, version, arches) {
  const zipNames = [...new Set(arches)].map((arch) => `wisp-${version}-${arch}.zip`);
  if (zipNames.length === 0) {
    throw new Error('name at least one arch');
  }
  const expected = ['wisp.rb', ...zipNames].sort();
  const actual = readdirSync(dir).sort();
  if (actual.join('\n') !== expected.join('\n')) {
    throw new Error(`${dir} holds ${actual.join(', ') || 'nothing'}; expected exactly ${expected.join(', ')}`);
  }
  for (const name of actual) {
    if (!lstatSync(path.join(dir, name)).isFile()) {
      throw new Error(`${name} in ${dir} is not a regular file`);
    }
  }
  const zips = zipNames.map((name) => path.join(dir, name));
  if (caskFromZips(version, zips) !== readFileSync(path.join(dir, 'wisp.rb'), 'utf8')) {
    throw new Error(`wisp.rb in ${dir} does not match the cask generated from its zips`);
  }
  return zips.map((zip) => `${createHash('sha256').update(readFileSync(zip)).digest('hex')}  ${path.basename(zip)}`);
}

module.exports = { checkArtifact };
