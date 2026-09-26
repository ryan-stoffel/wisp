// Creating a project from a temp git repo, seeing it in the sidebar, and it surviving a relaunch:
// wispd's project store is SQLite (daemon/src/store.rs), so a relaunch's fresh wispd lists it again.
import { TIMEOUT_MS, readWispdPid, ready, type Session } from '../../../screenshots/src/harness.ts';
import { answerFolderPicker } from '../../../screenshots/src/projects.ts';
import { check } from '../../../smoke/src/check.ts';
import { gitWorkspace } from '../../../smoke/src/harness.ts';
import { launchConnected } from '../harness.ts';
import { createLocalProject, waitForHostKind, waitForProjectRowSession, waitForSingleProjectRow } from '../wispUi.ts';

export const projectPersistenceChecks = [
  check('creating a project shows it in the sidebar and it survives a relaunch', async () => {
    const workspace = await gitWorkspace();
    let session: Session = await launchConnected();
    try {
      await ready(session);
      await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);

      await answerFolderPicker(session.app, workspace.folder);
      await createLocalProject(session.window);
      const created = await waitForSingleProjectRow(session.window, TIMEOUT_MS);
      const pidBefore = await readWispdPid(session.wispdDataDir);

      session = await session.relaunch();
      await waitForHostKind(session.window, ['connected'], TIMEOUT_MS);
      await waitForProjectRowSession(session.window, created, TIMEOUT_MS);

      // relaunch() stops the old serve before starting the app again, so a new wispd must have
      // answered project/list from its store on disk.
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
