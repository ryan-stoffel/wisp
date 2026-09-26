// M1's done-when: the editor's handshake with a real, bundled wispd leads to a connected host chip.
import { TIMEOUT_MS, ready } from '../../../screenshots/src/harness.ts';
import { check } from '../../../smoke/src/check.ts';
import { launchConnected } from '../harness.ts';
import { waitForHostKind } from '../wispUi.ts';

export const handshakeChecks = [
  check('a plain launch connects to the bundled wispd and shows a connected host chip', async () => {
    const session = await launchConnected();
    try {
      await ready(session);
      const status = await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);
      if (status.kind !== 'connected') {
        throw new Error(`host chip is ${JSON.stringify(status)}, not connected`);
      }
    } finally {
      await session.close();
    }
  }),
];
