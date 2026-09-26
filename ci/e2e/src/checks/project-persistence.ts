// Creating a project from a temp git repo, seeing it in the sidebar, and it surviving a relaunch.
// wispd's project store is SQLite (daemon/src/store.rs) even though M1's event log lives only in
// memory (0007's event_log.rs), so a relaunch's fresh wispd still lists the same project through a
// plain `project/list`, without needing the old event log to have survived.
import { check } from '../check.ts';
import { TIMEOUT_MS, launchConnected, projectWorkspace, readWispdPid, ready, type Session } from '../harness.ts';
import { projectRowSessions, waitForHostKind } from '../wispUi.ts';
import { createLocalProject, stubFolderPicker } from '../wispProject.ts';

export const projectPersistenceChecks = [
  check('creating a project shows it in the sidebar and it survives a relaunch', async () => {
    const workspace = await projectWorkspace();
    let session: Session = await launchConnected();
    try {
      await ready(session);
      await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);

      await stubFolderPicker(session.app, workspace.folder);
      await createLocalProject(session.window);

      await session.window.waitForFunction(
        () => document.querySelectorAll('.wisp-threads-rows button.wisp-threads-row').length > 0,
        undefined,
        { timeout: TIMEOUT_MS },
      );
      const created = await projectRowSessions(session.window);
      if (created.length !== 1) {
        throw new Error(`expected one project row after creating it, found ${String(created.length)}: ${created.join(', ')}`);
      }
      const pidBefore = await readWispdPid(session.wispdDataDir);

      // relaunch()'s own type is the narrower ScenarioContext; at runtime it is always a full
      // Session (ci/screenshots/src/harness.ts's `start` returns one either way).
      session = (await session.relaunch()) as Session;
      await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);
      await session.window.waitForFunction(
        () => document.querySelectorAll('.wisp-threads-rows button.wisp-threads-row').length > 0,
        undefined,
        { timeout: TIMEOUT_MS },
      );
      const afterRelaunch = await projectRowSessions(session.window);
      if (afterRelaunch.length !== 1 || afterRelaunch[0] !== created[0]) {
        throw new Error(`project list after relaunch is ${JSON.stringify(afterRelaunch)}, expected ${JSON.stringify(created)}`);
      }

      // Proves the list came from wispd's on-disk store, not from the old serve somehow surviving:
      // relaunch() SIGTERMs the old serve and waits for it to exit before starting the app again
      // (ci/screenshots/src/harness.ts's `quit`), so a new wispd must have answered project/list.
      const pidAfter = await readWispdPid(session.wispdDataDir);
      if (pidAfter === undefined || pidAfter === pidBefore) {
        throw new Error(`expected a new wispd process after relaunch; before ${String(pidBefore)}, after ${String(pidAfter)}`);
      }
    } finally {
      await session.close();
      await workspace.cleanup();
    }
  }),
];
