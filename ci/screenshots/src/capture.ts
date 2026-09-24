import { mkdir, readdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import {
  appLaunchOptions,
  isNotAvailable,
  launch,
  ready,
  screenshot,
  type LaunchOptions,
  type Scenario,
  type Session,
} from './harness.ts';
import { MANIFEST_FILE, writeManifest, type Manifest, type Result } from './manifest.ts';
import { scenarios } from './scenarios.ts';

const scenarioTimeoutMs = 180_000;

const outDir = resolve(process.argv[2] ?? join(import.meta.dirname, '..', 'out'));
await mkdir(outDir, { recursive: true });
for (const entry of await readdir(outDir)) {
  if (entry.endsWith('.png') || entry === MANIFEST_FILE) {
    await rm(join(outDir, entry));
  }
}

const manifest: Manifest = {
  results: scenarios.map(({ name, title }) => ({ name, title, status: 'pending' })),
};
await writeManifest(outDir, manifest);

try {
  const options = await appLaunchOptions();
  for (const [index, scenario] of scenarios.entries()) {
    console.log(`${scenario.name}: running`);
    const result = await capture(scenario, options);
    manifest.results[index] = result;
    await writeManifest(outDir, manifest);
    console.log(`${scenario.name}: ${describe(result)}`);
  }
} catch (error) {
  manifest.error = messageOf(error);
  await writeManifest(outDir, manifest);
  console.error(`capture stopped: ${manifest.error}`);
}

console.log(`wrote ${join(outDir, MANIFEST_FILE)}`);
if (manifest.error !== undefined || manifest.results.some((result) => result.status === 'failed')) {
  process.exitCode = 1;
}

async function capture(scenario: Scenario, options: LaunchOptions): Promise<Result> {
  const { name, title } = scenario;
  let session: Session | undefined;
  try {
    session = await launch(options, scenario.args ?? []);
    await ready(session.window);
    const shot = await withTimeout(scenario.run(session), scenarioTimeoutMs);
    if (isNotAvailable(shot)) {
      return { name, title, status: 'not-available', reason: shot.notAvailable };
    }
    const file = `${name}.png`;
    await writeFile(join(outDir, file), shot);
    return { name, title, status: 'captured', file };
  } catch (error) {
    const failed = session ? await screenshot(session.window).catch(() => undefined) : undefined;
    if (failed) {
      const file = `${name}.failed.png`;
      await writeFile(join(outDir, file), failed);
      return { name, title, status: 'failed', error: messageOf(error), file };
    }
    return { name, title, status: 'failed', error: messageOf(error) };
  } finally {
    await session?.close();
  }
}

async function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`the scenario did not finish within ${String(ms / 1000)} s`));
    }, ms);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

function describe(result: Result): string {
  switch (result.status) {
    case 'captured':
      return `captured ${result.file}`;
    case 'not-available':
      return `not available yet (${result.reason})`;
    case 'failed':
      return `failed: ${result.error}`;
    case 'pending':
      return 'did not run';
  }
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
