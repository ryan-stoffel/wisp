import type { Check } from './check.ts';
import { accountsChecks } from './checks/accounts.ts';
import { agentReviewChecks } from './checks/agent-review.ts';
import { agentsChecks } from './checks/agents.ts';
import { agentsWindowChecks } from './checks/agents-window.ts';
import { editorChecks } from './checks/editor.ts';
import { exclusionChecks } from './checks/exclusions.ts';
import { threadsChecks } from './checks/threads.ts';

const checks: readonly Check[] = [...agentsWindowChecks, ...accountsChecks, ...agentsChecks, ...agentReviewChecks, ...threadsChecks, ...editorChecks, ...exclusionChecks];

const names = new Set<string>();
for (const { name } of checks) {
  if (names.has(name)) {
    throw new Error(`check ${JSON.stringify(name)} is registered twice`);
  }
  names.add(name);
}

let failed = 0;
for (const { name, run } of checks) {
  const started = Date.now();
  try {
    await run();
    console.log(`PASS (${String(Date.now() - started)}ms) ${name}`);
  } catch (error) {
    failed += 1;
    console.log(`FAIL (${String(Date.now() - started)}ms) ${name}`);
    console.log(error instanceof Error ? (error.stack ?? error.message) : String(error));
  }
}

console.log(`${String(checks.length - failed)}/${String(checks.length)} checks passed`);
if (failed > 0) {
  process.exitCode = 1;
}
