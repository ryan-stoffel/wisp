// A normal thread end to end over the bundled wispd, with the fake Claude Code first on PATH: New
// Chat in a project's repository, the thread's row under Repositories, its chat, composer footer,
// and a follow-up message.
import { agentReply, chatShows, fakeClaudeEnv, sendMessage } from '../../../screenshots/src/agents.ts';
import { appLaunchOptions, launch, ready } from '../../../screenshots/src/harness.ts';
import { createProject, projectName } from '../../../screenshots/src/projects.ts';
import { openNewChat, repoThreadRow, sendFirstMessage, sessionsPart, threadTask } from '../../../screenshots/src/threads.ts';
import { check } from '../check.ts';

export const threadsChecks = [
  check('New Chat starts a normal thread that lists under its repository and takes a message', async () => {
    const options = await appLaunchOptions();
    let step = 'launch';
    const session = await launch({ ...options, env: { ...options.env, ...fakeClaudeEnv() } });
    try {
      const { window } = session;
      await ready(session);

      step = 'a project is created from a repository with a commit';
      await createProject(session, { withCommit: true });

      step = "New Chat opens the new-session composer in the project's repository";
      await openNewChat(window);
      const picker = await sessionsPart(window).textContent();
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
