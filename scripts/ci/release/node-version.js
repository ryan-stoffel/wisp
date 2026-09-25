'use strict';

const { readFileSync } = require('node:fs');
const path = require('node:path');

function requirePinnedNode(script) {
  const pinned = readFileSync(path.join(__dirname, '..', '..', '..', '.nvmrc'), 'utf8').trim();
  const want = pinned.split('.')[0];
  if (process.versions.node.split('.')[0] !== want) {
    process.stderr.write(`${script}: Node ${want} is required (.nvmrc pins ${pinned}), but node is ${process.version}\n`);
    process.exit(1);
  }
}

module.exports = { requirePinnedNode };
