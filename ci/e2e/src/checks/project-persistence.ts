// Creating a project from a temp git repo, seeing it in the sidebar, and it surviving a relaunch.
// wispd's project store is SQLite (daemon/src/store.rs) even though M1's event log lives only in
// memory (0007's event_log.rs), so a relaunch's fresh wispd still lists the same project through a
// plain `project/list`, without needing the old event log to have survived.
import { check } from '../check.ts';
import { TIMEOUT_MS, launchConnected, projectWorkspace, readWispdPid, ready, type Session } from '../harness.ts';
import { waitForHostKind, waitForProjectRowSession, waitForSingleProjectRow } from '../wispUi.ts';
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
      const created = await waitForSingleProjectRow(session.window, TIMEOUT_MS);
      const pidBefore = await readWispdPid(session.wispdDataDir);

      // relaunch()'s own type is the narrower ScenarioContext; at runtime it is always a full
      // Session (ci/screenshots/src/harness.ts's `start` returns one either way).
      session = (await session.relaunch()) as Session;
      await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);
      await waitForProjectRowSession(session.window, created, TIMEOUT_MS);

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
