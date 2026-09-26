export interface Check {
  readonly name: string;
  readonly run: () => Promise<void>;
}

export function check(name: string, run: () => Promise<void>): Check {
  return { name, run };
}

/** Thrown by a check that can't run here; reported as SKIP rather than PASS or FAIL. */
export class Skipped extends Error { }

export function skip(reason: string): never {
  throw new Skipped(reason);
}

/** Runs every check in order, logs PASS, SKIP, or FAIL for each, and sets a failing exit code if any failed. */
export async function runChecks(checks: readonly Check[]): Promise<void> {
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

  console.log(`${String(checks.length - failed - skipped)}/${String(checks.length)} checks passed${skipped > 0 ? `, ${String(skipped)} skipped` : ''}`);
  if (failed > 0) {
    process.exitCode = 1;
  }
}
