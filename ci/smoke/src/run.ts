import { runChecks } from './check.ts';
import { accountsChecks } from './checks/accounts.ts';
import { agentReviewChecks } from './checks/agent-review.ts';
import { agentsChecks } from './checks/agents.ts';
import { agentsWindowChecks } from './checks/agents-window.ts';
import { editorChecks } from './checks/editor.ts';
import { exclusionChecks } from './checks/exclusions.ts';
import { threadsChecks } from './checks/threads.ts';

await runChecks([...agentsWindowChecks, ...accountsChecks, ...agentsChecks, ...agentReviewChecks, ...threadsChecks, ...editorChecks, ...exclusionChecks]);
