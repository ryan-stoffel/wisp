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
//
// Beyond the sidebar row, this also asserts, through a second connection (queryProjectsOverSsh),
// that the ssh-side wispd's own store holds exactly the project just created, and that this Mac's
// own wispd (session.wispdDataDir) does not: not just that the editor believes it, but that the
// project actually landed on the host the user chose. #219 found and fixed a race where a project
// created right after a host switch could be created on the old host's wispd instead; this is the
// regression test for that fix (#224). If either assertion fails, saveWispdDiagnostics saves both
// data folders' logs and a listing of every `wispd serve` process for CI to upload.
import { skip, check } from '../check.ts';
import {
  TIMEOUT_MS,
  launchConnectedForSsh,
  projectWorkspace,
  queryProjectsDirect,
  queryProjectsOverSsh,
  readEnvFile,
  ready,
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

      // Waits for both the name and the kind together: since #219's fix, the chip shows
      // 'connecting' to the new host name (not a stale 'connected' to the *old* host, "this Mac")
      // while the shared process catches up to the switch. Waiting on the pair, not kind alone,
      // is what proves the chip is connected specifically to localhost.
      const connected = await waitForHostNamed(window, 'localhost', 'connected', TIMEOUT_MS);
      if (connected.kind !== 'connected' || connected.name !== 'localhost') {
        throw new Error(`host chip is ${JSON.stringify(connected)}, expected it connected to localhost`);
      }

      // The remote (typed-path) flow, not the native dialog: wispNewProject.ts only offers a
      // folder picker for "this Mac" (isLocalHost), and this host is now "localhost" (0007, #67).
      // waitForSingleProjectRow reads the row from the exact page evaluation that found it, so this
      // proves the editor believes wispd, reached over this real ssh connection, created and is
      // serving the project. The ground truth below proves the ssh-side wispd's own store agrees.
      await createRemoteProject(window, workspace.folder);
      const dataSession = await waitForSingleProjectRow(window, TIMEOUT_MS);
      const id = projectIdFromSession(dataSession);

      // Ground truth (#219, #224): a second connection to each wispd's own store, bypassing the
      // editor entirely. Exactly one project, and it's on the host the user actually chose.
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
