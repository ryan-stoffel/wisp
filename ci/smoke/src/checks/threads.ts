// A normal thread end to end (#110) over the local host's real wispd, the packaged app's bundled
// binary, with the fake Claude Code of ../../../screenshots/src/agents.ts first on PATH. With a project
// open, New Chat opens upstream's new-session composer in the project's repository; the first message
// starts a thread, which the sidebar lists under the repository in Repositories, and whose chat shows the
// agent's work, its composer, and its branch and host in the composer footer.
import { agentReply, chatShows, fakeClaudeEnv, sendMessage } from '../../../screenshots/src/agents.ts';
import { launch, type LaunchOptions } from '../../../screenshots/src/harness.ts';
import { createProject, projectName } from '../../../screenshots/src/projects.ts';
import { openNewChat, repoThreadRow, sendFirstMessage, threadTask } from '../../../screenshots/src/threads.ts';
import { check } from '../check.ts';
import { appLaunchOptions, ready } from '../harness.ts';

export const threadsChecks = [
  check('New Chat starts a normal thread that lists under its repository and takes a message', async () => {
    const options = await appLaunchOptions();
    const withFakeCli: LaunchOptions = { ...options, env: { ...options.env, ...fakeClaudeEnv() } };
    let step = 'launch';
    const session = await launch(withFakeCli);
    try {
      const { window } = session;
      await ready(session);

      step = 'a project is created from a repository with a commit';
      await createProject(session, { withCommit: true });

      step = "New Chat opens the new-session composer in the project's repository";
      await openNewChat(window);
      const picker = await window.locator('.part.sessions').last().textContent();
      if (!picker?.includes(projectName)) {
        throw new Error(`the composer's workspace is not ${projectName}: ${JSON.stringify(picker?.slice(0, 400))}`);
      }

      step = 'the first message starts a thread, listed under its repository in Repositories';
      await sendFirstMessage(window);
      const row = repoThreadRow(window);
      await row.waitFor({ state: 'visible', timeout: 30_000 });
      const repositories = await window.locator('.part.sidebar .wisp-threads-thread-section:not([hidden]) h2').allTextContents();
      if (!repositories.includes('Repositories')) {
        throw new Error(`the sidebar shows ${JSON.stringify(repositories)}, not Repositories`);
      }

      step = "the thread's chat shows the agent's work, and its footer the branch and host";
      await chatShows(window, agentReply);
      const footer = await window.locator('.wisp-composer-footer-branch').last().getAttribute('aria-label');
      if (!footer?.startsWith('Branch: wisp/')) {
        throw new Error(`the composer footer's branch is ${JSON.stringify(footer)}`);
      }

      step = 'a message from the composer reaches the agent, which answers it';
      await sendMessage(window, 'also cover the reconnect path');
      await chatShows(window, 'Fake agent heard: also cover the reconnect path');

      step = 'the row is still there, with the thread\'s title';
      const label = await row.getAttribute('aria-label');
      if (!label?.startsWith(`${threadTask}, chat in ${projectName}, updated`)) {
        throw new Error(`the row is labeled ${JSON.stringify(label)}`);
      }
    } catch (error) {
      throw new Error(`${step}: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
    } finally {
      await session.close();
    }
  }),
];
