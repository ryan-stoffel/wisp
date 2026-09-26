// The host switch, handshake, and project creation through `ssh localhost` (#66, #95): once
// scripts/ci/ssh-localhost has set up key-based ssh to localhost with its own throwaway key and
// known_hosts, switching `wisp.host` to `localhost` through the same UI a person would use should
// reach a real wispd over ssh, and creating a project there (through the remote, typed-path flow,
// since v1 can't browse a host's folders, #67) should show up in the sidebar (0007's transport is
// ssh either way, so this is the same coverage as the local project-persistence check).
//
// scripts/ci/ssh-localhost is skipped outright on anything but a macOS GitHub Actions runner, and
// even there only claims success once it has actually run `ssh localhost` itself. Locally, or if
// the runner's ssh setup ever breaks, this check skips too, with a clear reason (#95's own AC).
// But `ci.yml`'s `e2e` job sets WISP_E2E_REQUIRE_SSH=1: there, "not ready" fails instead of
// skipping, so a broken ssh setup fails the build instead of quietly dropping this coverage.
import { skip, check } from '../check.ts';
import {
  TIMEOUT_MS,
  launchConnectedForSsh,
  projectWorkspace,
  queryProjectsOverSsh,
  readEnvFile,
  ready,
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
/** Set by `ci.yml`'s `e2e` job only: makes "ssh not ready" a failure instead of a skip. */
const REQUIRE_SSH_ENV_VAR = 'WISP_E2E_REQUIRE_SSH';

export const sshChecks = [
  check('switching to ssh localhost creates a project and reaches it over a real ssh connection', async () => {
    const required = process.env[REQUIRE_SSH_ENV_VAR] === '1';
    const statusFile = process.env[STATUS_ENV_VAR];
    if (!statusFile) {
      const reason = `${STATUS_ENV_VAR} is not set; scripts/ci/ssh-localhost did not run (see #95)`;
      if (required) {
        throw new Error(`${REQUIRE_SSH_ENV_VAR}=1 but ssh localhost is not ready: ${reason}`);
      }
      skip(reason);
    }
    const status = await readEnvFile(statusFile);
    if (status.WISP_E2E_SSH_READY !== 'true') {
      const reason = status.WISP_E2E_SSH_REASON ?? `${statusFile} does not say ssh to localhost is ready (see #95)`;
      if (required) {
        throw new Error(`${REQUIRE_SSH_ENV_VAR}=1 but ssh localhost is not ready: ${reason}`);
      }
      skip(reason);
    }

    const workspace = await projectWorkspace();
    const sshLaunch = await launchConnectedForSsh();
    const { session } = sshLaunch;
    try {
      const { app, window } = session;
      await ready({ app, window });

      await openHostMenu(window);
      await clickHostMenuItem(window, 'add');
      await fillAddHostInput(window, 'localhost');
      await window.keyboard.press('Enter');

      // Waits for both the name and the kind together: right after the switch the chip can still
      // read 'connected' for the *old* host ("this Mac") for a moment, and a kind-only wait would
      // return on that stale match instead of the new host actually connecting.
      const connected = await waitForHostNamed(window, 'localhost', 'connected', TIMEOUT_MS);
      if (connected.kind !== 'connected' || connected.name !== 'localhost') {
        throw new Error(`host chip is ${JSON.stringify(connected)}, expected it connected to localhost`);
      }

      // The remote (typed-path) flow, not the native dialog: wispNewProject.ts only offers a
      // folder picker for "this Mac" (isLocalHost), and this host is now "localhost" (0007, #67).
      await createRemoteProject(window, workspace.folder);
      const rowSession = await waitForSingleProjectRow(window, TIMEOUT_MS);

      // Ground truth from the ssh-side wispd itself, asked the same way the editor reaches it
      // (over ssh, through the same wrapper): proves the project wispd created is the one the
      // sidebar shows.
      const projectId = projectIdFromSession(rowSession);
      const direct = await queryProjectsOverSsh(sshLaunch.wrapperPath);
      const directProject = direct[0];
      if (direct.length !== 1 || directProject?.id !== projectId) {
        throw new Error(`the ssh-side wispd lists ${JSON.stringify(direct)}, expected exactly the one project ${projectId}`);
      }
    } finally {
      await session.close();
      await sshLaunch.close();
      await workspace.cleanup();
    }
  }),
];
