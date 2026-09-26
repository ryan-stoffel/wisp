// A subagent end to end (#105) over the local host's real wispd, the packaged app's bundled binary. The
// agent is ci/smoke/fixtures/fake-cli/claude, first on PATH, which wispd detects and runs as Claude Code:
// it answers a task by writing FAKE_AGENT_NOTES.md in its worktree, and a follow-up (a resumed session)
// by echoing it. The check starts one with Wisp: Start Subagent, sees the Agents pill count it, opens its
// row from the Agents panel, sees its output in its tab, and sends it a follow-up with the composer.
import { agentReply, agentsPill, agentTab, agentTask, chatShows, fakeClaudeEnv, openAgentsPanel, sendMessage, startSubagent } from '../../../screenshots/src/agents.ts';
import { launch, type LaunchOptions } from '../../../screenshots/src/harness.ts';
import { createProject, projectName } from '../../../screenshots/src/projects.ts';
import { check } from '../check.ts';
import { appLaunchOptions, ready } from '../harness.ts';

export const agentsChecks = [
  check('a subagent shows behind the Agents pill, opens as a tab, and takes a message', async () => {
    const options = await appLaunchOptions();
    const withFakeCli: LaunchOptions = { ...options, env: { ...options.env, ...fakeClaudeEnv() } };
    let step = 'launch';
    const session = await launch(withFakeCli);
    try {
      const { window } = session;
      await ready(session);

      step = 'a project is created from a repository with a commit';
      await createProject(session, { withCommit: true });

      step = 'Wisp: Start Subagent starts one and opens its tab next to the coordinator';
      await startSubagent(window);
      const tabs = await window.locator('[role="tab"]').allTextContents();
      if (!tabs.some((tab) => tab.includes(projectName))) {
        throw new Error(`the coordinator's tab is gone: ${JSON.stringify(tabs)}`);
      }

      step = "the subagent's tab shows its output and its composer";
      await chatShows(window, agentReply);
      const placeholder = await window.locator('.interactive-input-part .monaco-editor .view-lines').last().textContent();
      const composer = await window.locator('.interactive-input-part').last().textContent();
      if (!`${placeholder ?? ''}${composer ?? ''}`.includes('Message this agent')) {
        throw new Error(`the composer does not say "Message this agent": ${JSON.stringify(composer)}`);
      }

      step = 'the Agents pill counts it, and its panel lists it with its state and where it runs';
      await openAgentsPanel(window, projectName);
      const pillLabel = await agentsPill(window).getAttribute('aria-label');
      if (pillLabel !== 'Agents, 1, 0 running') {
        throw new Error(`the pill is labeled ${JSON.stringify(pillLabel)}`);
      }
      const row = window.locator('.wisp-agents-panel-row').first();
      await window.locator('.wisp-agents-panel-row[aria-label*="Needs review"]').waitFor({ state: 'visible', timeout: 30_000 });
      const rowLabel = await row.getAttribute('aria-label');
      if (rowLabel !== `${agentTask}, Needs review, on this Mac`) {
        throw new Error(`the row is labeled ${JSON.stringify(rowLabel)}`);
      }

      step = 'Enter on the row opens its tab again';
      await window.keyboard.press('Enter');
      await window.locator('.wisp-agents-panel').waitFor({ state: 'hidden' });
      await agentTab(window).waitFor({ state: 'visible' });
      await chatShows(window, agentReply);

      step = 'a message from the composer reaches the agent, which answers it';
      await sendMessage(window, 'also handle credit notes');
      await chatShows(window, 'Fake agent heard: also handle credit notes');
    } catch (error) {
      throw new Error(`${step}: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
    } finally {
      await session.close();
    }
  }),
];
