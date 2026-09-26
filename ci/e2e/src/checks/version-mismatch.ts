// A fake wispd that answers `initialize` with a higher protocol version shows the incompatible
// state (wispdClient.ts's handshake, 0007's versioning rules).
import { join } from 'node:path';
import { check } from '../check.ts';
import { TIMEOUT_MS, launchWithWispd, ready } from '../harness.ts';
import { waitForHostKind } from '../wispUi.ts';

const fakeWispd = join(import.meta.dirname, '..', '..', 'fixtures', 'fake-wispd-incompatible.mjs');

export const versionMismatchChecks = [
  check('a wispd speaking a newer protocol shows the incompatible state', async () => {
    const session = await launchWithWispd(fakeWispd);
    try {
      const { app, window } = session;
      await ready({ app, window });

      const status = await waitForHostKind(window, ['error'], TIMEOUT_MS);
      if (status.kind !== 'error' || !status.state.toLowerCase().includes('update')) {
        throw new Error(`host chip is ${JSON.stringify(status)}, expected the incompatible ("needs an update") state`);
      }
    } finally {
      await session.close();
    }
  }),
];
