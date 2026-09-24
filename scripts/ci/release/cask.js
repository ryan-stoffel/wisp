'use strict';

const { createHash } = require('node:crypto');
const { readFileSync } = require('node:fs');
const path = require('node:path');

const TEMPLATE_PATH = path.join(__dirname, 'wisp.rb.template');
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA256 = /^[0-9a-f]{64}$/;
const ZIP_NAME = /^wisp-(.+)-([^-]+)\.zip$/;
const HOMEBREW_ARCH = { arm64: 'arm', x64: 'intel' };

function stanzas(version, sha256ByArch) {
  if (!VERSION.test(version)) {
    throw new Error(`version must be MAJOR.MINOR.PATCH, got '${version}'`);
  }
  const arches = Object.keys(sha256ByArch);
  for (const arch of arches) {
    if (!HOMEBREW_ARCH[arch]) {
      throw new Error(`unknown arch '${arch}'; expected one of ${Object.keys(HOMEBREW_ARCH).join(', ')}`);
    }
    if (!SHA256.test(sha256ByArch[arch])) {
      throw new Error(`sha256 for ${arch} is not 64 lowercase hex characters`);
    }
  }
  if (!arches.includes('arm64')) {
    throw new Error('an arm64 zip is required');
  }
  if (arches.length === 1) {
    return {
      version,
      arch: 'arch arm: "arm64"',
      sha256: `sha256 "${sha256ByArch.arm64}"`,
      depends_on_arch: 'depends_on arch: :arm64',
    };
  }
  return {
    version,
    arch: 'arch arm: "arm64", intel: "x64"',
    sha256: `sha256 arm:   "${sha256ByArch.arm64}",\n       intel: "${sha256ByArch.x64}"`,
    depends_on_arch: '',
  };
}

function render(template, values) {
  const lookup = (name) => {
    if (!Object.hasOwn(values, name)) {
      throw new Error(`the template uses {{${name}}}, which has no value`);
    }
    return values[name];
  };
  const lines = [];
  for (const line of template.split('\n')) {
    const whole = /^( *)\{\{(\w+)\}\}$/.exec(line);
    if (whole) {
      const value = lookup(whole[2]);
      if (value !== '') {
        lines.push(...value.split('\n').map((part) => whole[1] + part));
      }
      continue;
    }
    lines.push(line.replace(/\{\{(\w+)\}\}/g, (_, name) => lookup(name)));
  }
  return lines.join('\n');
}

function renderCask(version, sha256ByArch, template = readFileSync(TEMPLATE_PATH, 'utf8')) {
  return render(template, stanzas(version, sha256ByArch));
}

function caskFromZips(version, files) {
  if (files.length === 0) {
    throw new Error('pass at least one zip');
  }
  const sha256ByArch = {};
  for (const file of files) {
    const match = ZIP_NAME.exec(path.basename(file));
    if (!match || match[1] !== version) {
      throw new Error(`${file} is not named wisp-${version}-<arch>.zip`);
    }
    if (Object.hasOwn(sha256ByArch, match[2])) {
      throw new Error(`more than one zip for ${match[2]}`);
    }
    sha256ByArch[match[2]] = createHash('sha256').update(readFileSync(file)).digest('hex');
  }
  return renderCask(version, sha256ByArch);
}

module.exports = { TEMPLATE_PATH, caskFromZips, render, renderCask };
