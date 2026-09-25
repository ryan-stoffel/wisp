'use strict';

const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const { mkdtempSync, readFileSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { after, describe, it } = require('node:test');
const { caskFromZips, render, renderCask } = require('./cask.js');

const ARM = 'a'.repeat(64);
const INTEL = 'b'.repeat(64);

function lines(text) {
  return text.split('\n');
}

describe('renderCask', () => {
  it('renders an arm64-only cask', () => {
    const cask = renderCask('0.1.0', { arm64: ARM });
    const all = lines(cask);
    assert.equal(all[0], 'cask "wisp" do');
    assert.ok(all.includes('  arch arm: "arm64"'));
    assert.ok(all.includes('  version "0.1.0"'));
    assert.ok(all.includes(`  sha256 "${ARM}"`));
    assert.ok(all.includes('  depends_on arch: :arm64'));
    assert.ok(all.includes('  app "Wisp.app"'));
    assert.ok(all.includes('  binary "#{appdir}/Wisp.app/Contents/Resources/app/bin/wisp"'));
    assert.ok(all.includes('  binary "#{appdir}/Wisp.app/Contents/Resources/app/bin/wispd"'));
    assert.ok(all.includes('  uninstall launchctl: "io.github.ryan-stoffel.wisp.wispd"'));
    assert.ok(
      all.includes(
        '  url "https://github.com/ryan-stoffel/wisp/releases/download/v#{version}/wisp-#{version}-#{arch}.zip"',
      ),
    );
  });

  it('renders both arches with a sha256 per arch and no arch dependency', () => {
    const cask = renderCask('1.2.3', { arm64: ARM, x64: INTEL });
    assert.match(cask, /^ {2}arch arm: "arm64", intel: "x64"$/m);
    assert.match(cask, new RegExp(`^ {2}sha256 arm: {3}"${ARM}",\\n {9}intel: "${INTEL}"$`, 'm'));
    assert.doesNotMatch(cask, /depends_on arch/);
  });

  it('leaves no placeholders, trailing spaces, or doubled blank lines', () => {
    for (const cask of [renderCask('0.1.0', { arm64: ARM }), renderCask('0.1.0', { arm64: ARM, x64: INTEL })]) {
      assert.doesNotMatch(cask, /\{\{|\}\}/);
      assert.doesNotMatch(cask, /[ \t]$/m);
      assert.doesNotMatch(cask, /\n\n\n/);
      assert.ok(cask.endsWith('end\n'));
    }
  });

  // The app's name, its launcher, and its data folders all follow editor/product.json.
  it('installs and zaps what editor/product.json builds', () => {
    const product = JSON.parse(readFileSync(path.join(__dirname, '../../../editor/product.json'), 'utf8'));
    const all = lines(renderCask('0.1.0', { arm64: ARM }));
    const app = `${product.nameLong}.app`;
    assert.ok(all.includes(`  app "${app}"`));
    assert.ok(all.includes(`  binary "#{appdir}/${app}/Contents/Resources/app/bin/${product.applicationName}"`));
    // wispd's LaunchAgent (docs/decisions/0010, #61): the cask must unload it before Homebrew
    // removes the binary it points at, or uninstalling leaves a broken LaunchAgent behind.
    assert.ok(all.includes('  uninstall launchctl: "io.github.ryan-stoffel.wisp.wispd"'));
    assert.ok(all.includes(`      xattr -dr com.apple.quarantine #{appdir}/${app}`));

    const zap = all.slice(all.indexOf('  zap trash: ['), all.indexOf('  ]'));
    const globs = zap.map((line) => /^ {4}"([^"]+)",$/.exec(line)?.[1]).filter(Boolean);
    const escaped = (glob) => glob.replace(/[.+^$(){}|\\]/g, '\\$&').replace(/\*/g, '[^/]*');
    const covers = (glob, file) => new RegExp(`^${escaped(glob)}$`).test(file);
    const id = product.darwinBundleIdentifier;
    for (const file of [
      `~/${product.dataFolderName}`,
      `~/${product.sharedDataFolderName}`,
      `~/Library/Application Support/${product.nameShort}`,
      // wispd's data folder (docs/decisions/0009)
      '~/Library/Application Support/wisp',
      `~/Library/Caches/${id}`,
      `~/Library/HTTPStorages/${id}`,
      // wispd's LaunchAgent plist (docs/decisions/0010, #61)
      '~/Library/LaunchAgents/io.github.ryan-stoffel.wisp.wispd.plist',
      `~/Library/Preferences/${id}.plist`,
      `~/Library/Saved Application State/${id}.savedState`,
    ]) {
      assert.ok(
        globs.some((glob) => covers(glob, file)),
        `zap does not cover ${file}`,
      );
    }
  });

  it('rejects inputs that would produce a broken cask', () => {
    assert.throws(() => renderCask('v0.1.0', { arm64: ARM }), /MAJOR\.MINOR\.PATCH/);
    assert.throws(() => renderCask('0.1', { arm64: ARM }), /MAJOR\.MINOR\.PATCH/);
    assert.throws(() => renderCask('0.1.0', { x64: INTEL }), /arm64 zip is required/);
    assert.throws(() => renderCask('0.1.0', { arm64: ARM, ppc: INTEL }), /unknown arch 'ppc'/);
    assert.throws(() => renderCask('0.1.0', { arm64: 'ABC' }), /64 lowercase hex/);
  });
});

describe('render', () => {
  it('fails on a placeholder that has no value', () => {
    assert.throws(() => render('cask "{{token}}" do\nend\n', {}), /\{\{token\}\}/);
  });

  it('drops a whole-line placeholder whose value is empty', () => {
    assert.equal(render('a\n  {{gone}}\nb\n', { gone: '' }), 'a\nb\n');
  });
});

describe('caskFromZips', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'generate-cask-test-'));
  after(() => rmSync(dir, { recursive: true, force: true }));
  function zip(name, content) {
    const file = path.join(dir, name);
    writeFileSync(file, content);
    return file;
  }
  const sha = (content) => createHash('sha256').update(content).digest('hex');

  it('hashes each zip into the stanza for its arch', () => {
    const cask = caskFromZips('0.2.0', [zip('wisp-0.2.0-arm64.zip', 'arm'), zip('wisp-0.2.0-x64.zip', 'intel')]);
    assert.match(cask, new RegExp(`arm: {3}"${sha('arm')}"`));
    assert.match(cask, new RegExp(`intel: "${sha('intel')}"`));
    assert.match(cask, /^ {2}version "0\.2\.0"$/m);
  });

  it('rejects zips whose name does not match the version', () => {
    assert.throws(() => caskFromZips('0.2.0', [zip('wisp-0.1.0-arm64.zip', 'x')]), /wisp-0\.2\.0-<arch>\.zip/);
    assert.throws(() => caskFromZips('0.2.0', [zip('wisp.zip', 'x')]), /wisp-0\.2\.0-<arch>\.zip/);
  });

  it('rejects two zips for one arch and an empty list', () => {
    const file = zip('wisp-0.3.0-arm64.zip', 'x');
    assert.throws(() => caskFromZips('0.3.0', [file, file]), /more than one zip for arm64/);
    assert.throws(() => caskFromZips('0.3.0', []), /at least one zip/);
  });
});
