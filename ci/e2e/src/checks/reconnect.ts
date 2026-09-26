// Killing wispd mid-session: the chip shows disconnected, then reconnects on its own (the client's
// backoff, wispdClient.ts), and the sidebar never doubles the project it already had.
import { TIMEOUT_MS, appLaunchOptions, readWispdPid, ready } from '../../../screenshots/src/harness.ts';
import { answerFolderPicker } from '../../../screenshots/src/projects.ts';
import { check } from '../../../smoke/src/check.ts';
import { gitWorkspace } from '../../../smoke/src/harness.ts';
import { bundledWispd } from '../../../smoke/src/wispd.ts';
import { killWispd, launchConnected, queryProjectsDirect } from '../harness.ts';
import { createLocalProject, projectIdFromSession, waitForHostKind, waitForProjectRowSession, waitForSingleProjectRow } from '../wispUi.ts';

export const reconnectChecks = [
  check('killing wispd mid-session shows disconnected, then reconnects with no duplicate projects', async () => {
    const workspace = await gitWorkspace();
    const session = await launchConnected();
    try {
      const { app, window } = session;
      await ready(session);
      await waitForHostKind(window, ['connected'], TIMEOUT_MS);

      await answerFolderPicker(app, workspace.folder);
      await createLocalProject(window);
      const beforeSession = await waitForSingleProjectRow(window, TIMEOUT_MS);
      const projectId = projectIdFromSession(beforeSession);
      const pidBefore = await readWispdPid(session.wispdDataDir);

      await killWispd(session);

      // wispHostStatus.ts reports a lost, still-retrying connection as 'error' until it reconnects.
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

      // Ground truth first: the editor keeps its old list until the resubscribe's resync lands, so
      // a connected chip does not yet prove its list has caught up. wispd's own store has no lag.
      const direct = await queryProjectsDirect(bundledWispd(await appLaunchOptions()), session.wispdDataDir);
      const directProject = direct[0];
      if (direct.length !== 1 || directProject?.id !== projectId) {
        throw new Error(`wispd itself lists ${JSON.stringify(direct)}, expected exactly the one project ${projectId}`);
      }

      await waitForProjectRowSession(window, beforeSession, TIMEOUT_MS);
    } finally {
      await session.close();
      await workspace.cleanup();
    }
  }),
];
