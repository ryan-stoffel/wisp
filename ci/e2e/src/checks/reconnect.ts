// Killing wispd mid-session: the chip shows disconnected, then reconnects on its own (the client's
// backoff, wispdClient.ts), and the sidebar never doubles the project it already had (0007's
// idempotent create plus a plain `project/list` on the new connection).
import { check } from '../check.ts';
import {
  TIMEOUT_MS,
  appLaunchOptions,
  bundledWispdPath,
  killWispd,
  launchConnected,
  projectWorkspace,
  queryProjectsDirect,
  ready,
  readWispdPid,
} from '../harness.ts';
import { projectIdFromSession, projectRowSessions, waitForHostKind } from '../wispUi.ts';
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
      const beforeKill = await projectRowSessions(window);
      const beforeSession = beforeKill[0];
      if (beforeKill.length !== 1 || beforeSession === undefined) {
        throw new Error(`expected one project row before killing wispd, found ${String(beforeKill.length)}`);
      }
      const projectId = projectIdFromSession(beforeSession);
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

      // Ground truth first: WispProjectsService (wispProjectsService.ts) keeps its old array across
      // a reconnect until the resubscribe's resync lands and it re-lists, so the chip alone saying
      // 'connected' does not prove the editor's own list has caught up yet. wispd's own store has
      // no such lag once it has answered project/list at all.
      const wispdPath = bundledWispdPath(await appLaunchOptions());
      const direct = await queryProjectsDirect(wispdPath, session.wispdDataDir);
      const directProject = direct[0];
      if (direct.length !== 1 || directProject?.id !== projectId) {
        throw new Error(`wispd itself lists ${JSON.stringify(direct)}, expected exactly the one project ${projectId}`);
      }

      // Then the UI: wait for it to catch up to that same single project rather than reading it once.
      await window.waitForFunction(
        ({ selector, expected }) => {
          const rows = [...document.querySelectorAll(selector)] as HTMLElement[];
          return rows.length === 1 && rows[0]?.dataset.session === expected;
        },
        { selector: '.wisp-threads-rows button.wisp-threads-row', expected: beforeSession },
        { timeout: TIMEOUT_MS },
      );
      const afterReconnect = await projectRowSessions(window);
      if (afterReconnect.length !== 1 || afterReconnect[0] !== beforeSession) {
        throw new Error(`project list after reconnect is ${JSON.stringify(afterReconnect)}, expected ${JSON.stringify(beforeKill)}`);
      }
    } finally {
      await session.close();
      await workspace.cleanup();
    }
  }),
];
