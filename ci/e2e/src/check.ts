export interface Check {
  readonly name: string;
  readonly run: () => Promise<void>;
}

export function check(name: string, run: () => Promise<void>): Check {
  return { name, run };
}

/** Thrown by a check that can't run here (ssh.ts, when scripts/ci/ssh-localhost never reported ready). */
export class Skipped extends Error { }

/** Ends the running check with a clear reason, reported as SKIP rather than PASS or FAIL. */
export function skip(reason: string): never {
  throw new Skipped(reason);
}
