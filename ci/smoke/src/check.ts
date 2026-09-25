export interface Check {
  readonly name: string;
  readonly run: () => Promise<void>;
}

export function check(name: string, run: () => Promise<void>): Check {
  return { name, run };
}
