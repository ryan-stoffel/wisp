import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { readCapture, readLogTail } from '../src/artifact.ts';

const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.from('rest of the image')]);
let root = '';
let count = 0;

before(async () => {
  root = await mkdtemp(join(tmpdir(), 'wisp-artifact-test-'));
});

after(async () => {
  await rm(root, { recursive: true, force: true });
});

async function captureDir(manifest: unknown, files: Record<string, Buffer | string> = {}): Promise<string> {
  count += 1;
  const dir = join(root, `capture-${String(count)}`);
  await mkdir(dir);
  await writeFile(join(dir, 'manifest.json'), typeof manifest === 'string' ? manifest : JSON.stringify(manifest));
  for (const [name, content] of Object.entries(files)) {
    await writeFile(join(dir, name), content);
  }
  return dir;
}

const captured = { results: [{ name: 'startup', title: 'Startup', status: 'captured', file: 'startup.png' }] };

test('returns the manifest and the PNGs it lists', async () => {
  const dir = await captureDir(
    {
      results: [
        ...captured.results,
        { name: 'chat', title: 'Chat', status: 'failed', error: 'boom', file: 'chat.failed.png' },
        { name: 'editor', title: 'Editor', status: 'not-available', reason: 'later' },
      ],
    },
    { 'startup.png': png, 'chat.failed.png': png, 'stray.png': png },
  );

  const capture = await readCapture(dir);

  assert.ok(capture);
  assert.deepEqual(capture.files, ['startup.png', 'chat.failed.png']);
  assert.equal(capture.manifest.results.length, 3);
});

test('a missing directory or manifest means capture did not run', async () => {
  assert.equal(await readCapture(join(root, 'nothing-here')), undefined);
  const empty = join(root, 'empty');
  await mkdir(empty);
  assert.equal(await readCapture(empty), undefined);
});

test('rejects files that are missing, not PNGs, or symlinks', async () => {
  await assert.rejects(readCapture(await captureDir(captured)), /startup\.png is listed .* not a regular file/);
  await assert.rejects(readCapture(await captureDir(captured, { 'startup.png': 'GIF89a' })), /not a PNG/);

  const linked = await captureDir(captured);
  await writeFile(join(root, 'outside.png'), png);
  await symlink(join(root, 'outside.png'), join(linked, 'startup.png'));
  await assert.rejects(readCapture(linked), /not a regular file/);
});

test('rejects a manifest that is not JSON, not a file, or not valid', async () => {
  await assert.rejects(readCapture(await captureDir('{not json')), /not valid JSON/);
  await assert.rejects(
    readCapture(await captureDir({ results: [{ name: '../x', title: 'X', status: 'pending' }] })),
    /name/,
  );

  const linked = join(root, 'linked-manifest');
  await mkdir(linked);
  await writeFile(join(root, 'elsewhere.json'), JSON.stringify(captured));
  await symlink(join(root, 'elsewhere.json'), join(linked, 'manifest.json'));
  await assert.rejects(readCapture(linked), /manifest\.json is not a regular file/);

  const linkedDir = join(root, 'linked-dir');
  await symlink(linked, linkedDir);
  await assert.rejects(readCapture(linkedDir), /is not a directory/);
});

test('reads only the end of a log and ignores anything but a regular file', async () => {
  const logs = join(root, 'logs');
  await mkdir(logs);
  await writeFile(join(logs, 'build.log'), `${'x'.repeat(100_000)}\nlast line\n`);
  await symlink('/etc/hosts', join(logs, 'capture.log'));

  const tail = await readLogTail(logs, 'build.log');

  assert.ok(tail?.endsWith('last line\n'));
  assert.ok((tail?.length ?? 0) <= 64 * 1024);
  assert.equal(await readLogTail(logs, 'capture.log'), undefined);
  assert.equal(await readLogTail(logs, 'missing.log'), undefined);
  await symlink(logs, join(root, 'logs-link'));
  assert.equal(await readLogTail(join(root, 'logs-link'), 'build.log'), undefined);
});
