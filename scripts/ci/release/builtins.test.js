'use strict';

const assert = require('node:assert/strict');
const { readFileSync, readdirSync } = require('node:fs');
const path = require('node:path');
const { it } = require('node:test');

// The release job has no node_modules, so the scripts it runs may only use Node built-ins and each other.
it('the release scripts import only Node built-ins and local files', () => {
  const ci = path.join(__dirname, '..');
  const files = [
    ...['check-release-artifact', 'generate-cask', 'next-version', 'publish-cask'].map((name) => path.join(ci, name)),
    ...readdirSync(__dirname)
      .filter((name) => name.endsWith('.js') && !name.endsWith('.test.js'))
      .map((name) => path.join(__dirname, name)),
  ];
  for (const file of files) {
    for (const [, specifier] of readFileSync(file, 'utf8').matchAll(/require\(\s*['"]([^'"]+)['"]\s*\)/g)) {
      assert.match(specifier, /^(node:|\.\/)/, `${path.relative(ci, file)} requires '${specifier}'`);
    }
  }
});
