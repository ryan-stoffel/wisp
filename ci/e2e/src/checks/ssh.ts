// The host switch plus handshake through `ssh localhost` (#95): once scripts/ci/ssh-localhost has
// set up key-based ssh to localhost with its own throwaway key and known_hosts, switching
// `wisp.host` to `localhost` through the same UI a person would use should reach a real wispd over
// ssh and land on the same connected chip as the local case (0007's transport is ssh either way).
//
// scripts/ci/ssh-localhost is skipped outright on anything but a macOS CI runner, and even there
// only claims success once it has actually run `ssh localhost` itself; this check trusts that and
// skips too; wherever it can't run, that keeps #95 open rather than failing the build (its own AC).
import { skip, check } from '../check.ts';
import { TIMEOUT_MS, launchConnectedForSsh, readEnvFile, ready } from '../harness.ts';
import { runCommand, fillQuickInput, waitForHostKind } from '../wispUi.ts';

/** Where `scripts/ci/ssh-localhost` writes its `KEY=value` result (also read by scripts/ci/e2e). */
const STATUS_ENV_VAR = 'WISP_E2E_SSH_STATUS_FILE';

export const sshChecks = [
  check('switching to ssh localhost reaches a real wispd over ssh', async () => {
    const statusFile = process.env[STATUS_ENV_VAR];
    if (!statusFile) {
      skip(`${STATUS_ENV_VAR} is not set; scripts/ci/ssh-localhost did not run (see #95)`);
    }
    const status = await readEnvFile(statusFile);
    if (status.WISP_E2E_SSH_READY !== 'true') {
      skip(status.WISP_E2E_SSH_REASON ?? `${statusFile} does not say ssh to localhost is ready (see #95)`);
    }

    // wisp.remoteWispdPath is pre-set to the bundled wispd's own path: a bare CI runner has no
    // Homebrew wispd on PATH for `ssh localhost` to find on itself (daemon/README.md).
    const session = await launchConnectedForSsh();
    try {
      const { app, window } = session;
      await ready({ app, window });
      await waitForHostKind(window, ['connected'], TIMEOUT_MS);

      await runCommand(window, 'Wisp: Switch Host...');
      await fillQuickInput(window, 'localhost');
      await window.keyboard.press('Enter');

      const connected = await waitForHostKind(window, ['connected'], TIMEOUT_MS);
      if (connected.kind !== 'connected' || connected.name !== 'localhost') {
        throw new Error(`host chip is ${JSON.stringify(connected)}, expected it connected to localhost`);
      }
    } finally {
      await session.close();
    }
  }),
];
