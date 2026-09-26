import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { test } from 'node:test';

const fixture = join(import.meta.dirname, '..', 'fixtures', 'fake-wispd-incompatible.mjs');

test('answers initialize with a protocol version above what the editor supports', async () => {
  const child = spawn(fixture, ['attach'], { stdio: ['pipe', 'pipe', 'inherit'] });
  try {
    const line = await new Promise<string>((resolve, reject) => {
      const rl = createInterface({ input: child.stdout });
      rl.once('line', resolve);
      child.once('error', reject);
      child.stdin.write(`${JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: { protocol: { min: 1, max: 1 }, client: { name: 'test', version: '0' }, capabilities: {} },
      })}\n`);
    });
    const response = JSON.parse(line) as { jsonrpc: string; id: number; result: { protocol: number; wispd: string } };
    assert.equal(response.jsonrpc, '2.0');
    assert.equal(response.id, 1);
    assert.ok(response.result.protocol > 1, `expected a protocol above 1, got ${String(response.result.protocol)}`);
    assert.equal(typeof response.result.wispd, 'string');
  } finally {
    child.stdin.end();
    child.kill();
  }
});

test('exits once its stdin ends, leaving no process behind', async () => {
  const child = spawn(fixture, ['attach'], { stdio: ['pipe', 'ignore', 'inherit'] });
  child.stdin.end();
  const [code] = await new Promise<[number | null]>((resolve) => {
    child.once('exit', (exitCode) => { resolve([exitCode]); });
  });
  assert.equal(code, 0);
});
