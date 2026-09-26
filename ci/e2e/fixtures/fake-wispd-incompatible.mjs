#!/usr/bin/env node
// A fake `wispd` for ci/e2e's version-mismatch check (platform/wisp/node/wispdService.ts's
// WISP_WISPD_PATH override runs this as `<this> attach`). It answers `initialize` with a protocol
// version above what the editor supports (wispdClient.ts's SUPPORTED_PROTOCOL.max, currently 1),
// so wispdClient.ts's handshake enters the `incompatible` state on its own, the same way a real,
// newer wispd would (0007) -- no error response needed, and no socket or bridging (0010): this is
// the whole of `wispd attach` that the handshake ever sees.
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

// wispd attach would exit once wispd or stdout closed (0010); this fake process does the same, so
// closing the app's end of the pipe (or the app closing wispd's stdio when it quits) never leaves
// this process behind.
process.stdin.on('end', () => {
  process.exit(0);
});
for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) {
  process.on(signal, () => {
    process.exit(0);
  });
}
