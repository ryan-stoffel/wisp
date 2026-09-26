// Switching `wisp.host` to `localhost` through the host menu reaches a real wispd over ssh, and a
// project created there (through the remote, typed-path flow) lands in the ssh-side wispd's own
// store and not in this Mac's: the regression test for a race where a project created right after
// a host switch went to the old host (#219, #224). Needs scripts/ci/ssh-localhost; skips without
// it, unless WISP_E2E_REQUIRE_SSH=1 (ci.yml's e2e job) makes that a failure.
import { TIMEOUT_MS, ready } from '../../../screenshots/src/harness.ts';
import { check, skip } from '../../../smoke/src/check.ts';
import { gitWorkspace } from '../../../smoke/src/harness.ts';
import {
  launchConnectedForSsh,
  queryProjectsDirect,
  queryProjectsOverSsh,
  readEnvFile,
  saveWispdDiagnostics,
} from '../harness.ts';
import {
  clickHostMenuItem,
  createRemoteProject,
  fillAddHostInput,
  openHostMenu,
  projectIdFromSession,
  waitForHostNamed,
  waitForSingleProjectRow,
} from '../wispUi.ts';

/** Where `scripts/ci/ssh-localhost` writes its `KEY=value` result (also read by scripts/ci/e2e). */
const STATUS_ENV_VAR = 'WISP_E2E_SSH_STATUS_FILE';
const REQUIRE_SSH_ENV_VAR = 'WISP_E2E_REQUIRE_SSH';

export const sshChecks = [
  check('switching to ssh localhost creates a project and reaches it over a real ssh connection', async () => {
    const statusFile = process.env[STATUS_ENV_VAR];
    const status = statusFile ? await readEnvFile(statusFile) : {};
    if (status.WISP_E2E_SSH_READY !== 'true') {
      const reason = statusFile
        ? (status.WISP_E2E_SSH_REASON ?? `${statusFile} does not say ssh to localhost is ready (see #95)`)
        : `${STATUS_ENV_VAR} is not set; scripts/ci/ssh-localhost did not run (see #95)`;
      if (process.env[REQUIRE_SSH_ENV_VAR] === '1') {
        throw new Error(`${REQUIRE_SSH_ENV_VAR}=1 but ssh localhost is not ready: ${reason}`);
      }
      skip(reason);
    }

    const workspace = await gitWorkspace();
    const sshLaunch = await launchConnectedForSsh();
    const { session } = sshLaunch;
    try {
      const { window } = session;
      await ready(session);

      await openHostMenu(window);
      await clickHostMenuItem(window, 'add');
      await fillAddHostInput(window, 'localhost');
      await window.keyboard.press('Enter');

      const connected = await waitForHostNamed(window, 'localhost', 'connected', TIMEOUT_MS);
      if (connected.kind !== 'connected' || connected.name !== 'localhost') {
        throw new Error(`host chip is ${JSON.stringify(connected)}, expected it connected to localhost`);
      }

      // Only "this Mac" gets a native folder picker; any other host asks for a typed path.
      await createRemoteProject(window, workspace.folder);
      const dataSession = await waitForSingleProjectRow(window, TIMEOUT_MS);
      const id = projectIdFromSession(dataSession);

      // Ground truth: a second connection to each wispd's own store, bypassing the editor.
      const sshProjects = await queryProjectsOverSsh(sshLaunch.wrapperPath);
      const sshProject = sshProjects[0];
      if (sshProjects.length !== 1 || sshProject?.id !== id) {
        await saveWispdDiagnostics({ ssh: sshLaunch.sshWispdDataDir, local: session.wispdDataDir });
        throw new Error(
          `the ssh-side wispd lists ${JSON.stringify(sshProjects)}, expected exactly the one project (${id})`,
        );
      }
      const localProjects = await queryProjectsDirect(sshLaunch.wispdPath, session.wispdDataDir);
      if (localProjects.some((project) => project.id === id)) {
        await saveWispdDiagnostics({ ssh: sshLaunch.sshWispdDataDir, local: session.wispdDataDir });
        throw new Error(
          `this Mac's own wispd (${session.wispdDataDir}) lists the project too: ${JSON.stringify(localProjects)}`,
        );
      }
    } finally {
      await session.close();
      await sshLaunch.close();
      await workspace.cleanup();
    }
  }),
];
