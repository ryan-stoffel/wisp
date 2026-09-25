// Killing wispd mid-session: the chip shows disconnected, then reconnects on its own (the client's
// backoff, wispdClient.ts), and the sidebar never doubles the project it already had (0007's
// idempotent create plus a plain `project/list` on the new connection).
import { check } from '../check.ts';
import { TIMEOUT_MS, killWispd, launchConnected, projectWorkspace, ready, readWispdPid } from '../harness.ts';
import { projectRowLabels, waitForHostKind } from '../wispUi.ts';
import { createLocalProject, stubFolderPicker } from '../wispProject.ts';

export const reconnectChecks = [
  check('killing wispd mid-session shows disconnected, then reconnects with no duplicate projects', async () => {
    const workspace = await projectWorkspace();
    const session = await launchConnected();
    try {
      const { app, window } = session;
      await ready({ app, window });
      await waitForHostKind(window, ['connected'], TIMEOUT_MS);

      await stubFolderPicker(app, workspace.folder);
      await createLocalProject(window);
      await window.waitForFunction(
        () => document.querySelectorAll('.wisp-threads-rows button.wisp-threads-row').length > 0,
        undefined,
        { timeout: TIMEOUT_MS },
      );
      const beforeKill = await projectRowLabels(window);
      if (beforeKill.length !== 1) {
        throw new Error(`expected one project row before killing wispd, found ${String(beforeKill.length)}`);
      }
      const pidBefore = await readWispdPid(session.wispdDataDir);

      await killWispd(session);

      // wispHostStatus.ts's describeProblem reports 'error' for the disconnected state itself and
      // for 'connecting' once it has a last problem to keep showing while it retries, so a lost,
      // still-retrying connection is 'error' throughout, right up to the moment it reconnects.
      const disconnected = await waitForHostKind(window, ['error'], TIMEOUT_MS);
      if (disconnected.kind !== 'error') {
        throw new Error(`host chip is ${JSON.stringify(disconnected)}, expected it to show a problem after wispd was killed`);
      }

      const reconnected = await waitForHostKind(window, ['connected'], TIMEOUT_MS);
      if (reconnected.kind !== 'connected') {
        throw new Error(`host chip is ${JSON.stringify(reconnected)}, not connected, after the reconnect window`);
      }
      const pidAfter = await readWispdPid(session.wispdDataDir);
      if (pidAfter === undefined || pidAfter === pidBefore) {
        throw new Error(`expected a new wispd process; before ${String(pidBefore)}, after ${String(pidAfter)}`);
      }

      const afterReconnect = await projectRowLabels(window);
      if (afterReconnect.length !== 1 || afterReconnect[0] !== beforeKill[0]) {
        throw new Error(`project list after reconnect is ${JSON.stringify(afterReconnect)}, expected ${JSON.stringify(beforeKill)}`);
      }
    } finally {
      await session.close();
      await workspace.cleanup();
    }
  }),
];
