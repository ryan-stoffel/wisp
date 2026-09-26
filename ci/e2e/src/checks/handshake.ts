// M1's done-when: the editor's handshake with a real, bundled wispd leads to a connected host chip.
import { check } from '../check.ts';
import { TIMEOUT_MS, launchConnected, ready } from '../harness.ts';
import { waitForHostKind } from '../wispUi.ts';

export const handshakeChecks = [
  check('a plain launch connects to the bundled wispd and shows a connected host chip', async () => {
    const session = await launchConnected();
    try {
      const { app, window } = session;
      await ready({ app, window });

      const status = await waitForHostKind(window, ['connected'], TIMEOUT_MS);
      if (status.kind !== 'connected') {
        throw new Error(`host chip is ${JSON.stringify(status)}, not connected`);
      }
    } finally {
      await session.close();
    }
  }),
];
