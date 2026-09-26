#!/usr/bin/env node
// A fake `wispd attach` for the version-mismatch check: it answers `initialize` with a protocol
// version above what the editor supports (wispdClient.ts's SUPPORTED_PROTOCOL.max, currently 1),
// as a real, newer wispd would (0007).
import { createInterface } from 'node:readline';

const rl = createInterface({ input: process.stdin, terminal: false });

rl.once('line', (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  const response = {
    jsonrpc: '2.0',
    id: request.id,
    result: {
      protocol: 2,
      wispd: 'fake-wispd-e2e',
      logId: '00000000-0000-7000-8000-000000000000',
      capabilities: {},
      maxFrameBytes: 8388608,
    },
  };
  process.stdout.write(`${JSON.stringify(response)}\n`);
});

// Like wispd attach, exit once the app closes its end, so this never outlives the app.
process.stdin.on('end', () => {
  process.exit(0);
});
for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) {
  process.on(signal, () => {
    process.exit(0);
  });
}
