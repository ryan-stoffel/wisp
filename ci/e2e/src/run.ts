import type { Check } from './check.ts';
import { Skipped } from './check.ts';
import { handshakeChecks } from './checks/handshake.ts';
import { projectPersistenceChecks } from './checks/project-persistence.ts';
import { reconnectChecks } from './checks/reconnect.ts';
import { sshChecks } from './checks/ssh.ts';
import { versionMismatchChecks } from './checks/version-mismatch.ts';

const checks: readonly Check[] = [
  ...handshakeChecks,
  ...projectPersistenceChecks,
  ...reconnectChecks,
  ...versionMismatchChecks,
  ...sshChecks,
];

const names = new Set<string>();
for (const { name } of checks) {
  if (names.has(name)) {
    throw new Error(`check ${JSON.stringify(name)} is registered twice`);
  }
  names.add(name);
}

let failed = 0;
let skipped = 0;
for (const { name, run } of checks) {
  const started = Date.now();
  try {
    await run();
    console.log(`PASS (${String(Date.now() - started)}ms) ${name}`);
  } catch (error) {
    if (error instanceof Skipped) {
      skipped += 1;
      console.log(`SKIP (${String(Date.now() - started)}ms) ${name}: ${error.message}`);
      continue;
    }
    failed += 1;
    console.log(`FAIL (${String(Date.now() - started)}ms) ${name}`);
    console.log(error instanceof Error ? (error.stack ?? error.message) : String(error));
  }
}

console.log(`${String(checks.length - failed - skipped)}/${String(checks.length)} checks passed, ${String(skipped)} skipped`);
if (failed > 0) {
  process.exitCode = 1;
}
