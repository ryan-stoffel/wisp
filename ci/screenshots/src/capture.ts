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
  type ScenarioContext,
  type Session,
} from './harness.ts';
import {
  capturedFile,
  failedFile,
  LIMITS,
  MANIFEST_FILE,
  nameProblem,
  textProblem,
  writeManifest,
  type Manifest,
  type Result,
} from './manifest.ts';
import { scenarios } from './scenarios.ts';

const scenarioTimeoutMs = 180_000;
const failureShotTimeoutMs = 10_000;

const outDir = resolve(process.argv[2] ?? join(import.meta.dirname, '..', 'out'));
await mkdir(outDir, { recursive: true });
for (const entry of await readdir(outDir)) {
  if (entry.endsWith('.png') || entry === MANIFEST_FILE) {
    await rm(join(outDir, entry));
  }
}

const manifest: Manifest = { results: [] };

try {
  checkScenarios(scenarios);
  manifest.results = scenarios.map(({ name, title }) => ({ name, title, status: 'pending' }));
  await writeManifest(outDir, manifest);
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

function checkScenarios(list: readonly Scenario[]): void {
  const seen = new Set<string>();
  for (const { name, title } of list) {
    const problem = nameProblem(name) ?? textProblem(title, LIMITS.title);
    if (problem) {
      throw new Error(`scenario ${JSON.stringify(name)} is invalid: its name or title ${problem}`);
    }
    if (seen.has(name)) {
      throw new Error(`scenario ${name} appears twice`);
    }
    seen.add(name);
  }
}

async function capture(scenario: Scenario, options: LaunchOptions): Promise<Result> {
  const { name, title } = scenario;
  let session: Session | undefined;
  // The window a failure is shot from: the newest, after any relaunch.
  let latest: ScenarioContext | undefined;
  const track = (context: ScenarioContext): ScenarioContext => {
    latest = context;
    return { ...context, relaunch: async () => track(await context.relaunch()) };
  };
  try {
    session = await launch(options, scenario);
    const context = track(session);
    const shot = await withTimeout(
      (async () => {
        await ready(context);
        return scenario.run(context);
      })(),
      scenarioTimeoutMs,
    );
    if (isNotAvailable(shot)) {
      const problem = textProblem(shot.notAvailable, LIMITS.reason);
      if (problem) {
        throw new Error(`notAvailable reason ${problem}: ${shot.notAvailable}`);
      }
      return { name, title, status: 'not-available', reason: shot.notAvailable };
    }
    await writeFile(join(outDir, capturedFile(name)), shot);
    return { name, title, status: 'captured', file: capturedFile(name) };
  } catch (error) {
    const failed = latest ? await screenshot(latest.window, failureShotTimeoutMs).catch(() => undefined) : undefined;
    if (failed) {
      await writeFile(join(outDir, failedFile(name)), failed);
      return { name, title, status: 'failed', error: messageOf(error), file: failedFile(name) };
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
  const message = error instanceof Error ? error.message : String(error);
  return message.length > LIMITS.error ? `${message.slice(0, LIMITS.error - 6)} [cut]` : message;
}
