import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs, promisify } from 'node:util';
import type { Video } from 'playwright-core';
import { MAX_BYTES } from './artifact.ts';
import {
  appLaunchOptions,
  isNotAvailable,
  launch,
  ready,
  screenshot,
  WINDOW_SIZE,
  type LaunchOptions,
  type Scenario,
  type ScenarioContext,
  type Session,
} from './harness.ts';
import {
  baseCapturedFile,
  baseFailedFile,
  capturedFile,
  failedFile,
  LIMITS,
  MANIFEST_FILE,
  nameProblem,
  textProblem,
  videoFile,
  writeManifest,
  type Manifest,
  type Result,
  type Shot,
} from './manifest.ts';
import { checkScenes, parseRequest, wants, type Entry } from './request.ts';
import { scenarios } from './scenarios.ts';

const scenarioTimeoutMs = 180_000;
const failureShotTimeoutMs = 10_000;
const videoSaveTimeoutMs = 60_000;
/** How long a video keeps showing the scene's last state before the app quits. */
const videoHoldMs = 2_000;
const maxGifBytes = MAX_BYTES.gif;
/** GIF settings to try in order until the preview fits under maxGifBytes. */
const gifAttempts = [
  { fps: 10, width: 800 },
  { fps: 8, width: 640 },
  { fps: 5, width: 480 },
];
const execFileAsync = promisify(execFile);

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: { request: { type: 'string' } },
});
const outDir = resolve(positionals[0] ?? join(import.meta.dirname, '..', 'out'));
await mkdir(outDir, { recursive: true });
for (const entry of await readdir(outDir)) {
  if (/\.(png|gif|webm)$/.test(entry) || entry === MANIFEST_FILE) {
    await rm(join(outDir, entry));
  }
}

const manifest: Manifest = { results: [] };

try {
  checkScenarios(scenarios);
  if (values.request === undefined) {
    throw new Error('no request: pass --request with the scenes to capture, as in --request "after: agents-window"');
  }
  const request = parseRequest(values.request);
  checkScenes(
    request,
    scenarios.map((scenario) => scenario.name),
  );
  const planned = request.entries.map((entry) => ({ entry, scenario: scenarioNamed(entry.name) }));
  manifest.results = planned.map(({ entry, scenario }) => pending(entry, scenario));
  await writeManifest(outDir, manifest);
  const head = await appLaunchOptions();
  const baseBundle = process.env.WISP_BASE_APP_BUNDLE;
  const base = wants(request, 'before-after') && baseBundle ? await appLaunchOptions(baseBundle) : undefined;
  for (const [index, { entry, scenario }] of planned.entries()) {
    console.log(`${entry.mode} ${scenario.name}: running`);
    const result = await captureEntry(entry, scenario, head, base);
    manifest.results[index] = result;
    await writeManifest(outDir, manifest);
    console.log(`${entry.mode} ${scenario.name}: ${describe(result)}${result.base ? `; base app: ${describe(result.base)}` : ''}`);
  }
} catch (error) {
  manifest.error = messageOf(error);
  await writeManifest(outDir, manifest);
  console.error(`capture stopped: ${manifest.error}`);
}

console.log(`wrote ${join(outDir, MANIFEST_FILE)}`);
// Only the head app's shots decide the exit code: a before-after scene is expected to look wrong, or even fail, on the base app.
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

function scenarioNamed(name: string): Scenario {
  const scenario = scenarios.find((candidate) => candidate.name === name);
  if (!scenario) {
    throw new Error(`no scenario is named ${name}`);
  }
  return scenario;
}

function pending(entry: Entry, scenario: Scenario): Result {
  const result: Result = { name: scenario.name, title: scenario.title, mode: entry.mode, status: 'pending' };
  if (entry.mode === 'before-after') {
    result.base = { status: 'pending' };
  }
  return result;
}

async function captureEntry(entry: Entry, scenario: Scenario, head: LaunchOptions, base: LaunchOptions | undefined): Promise<Result> {
  const { name, title } = scenario;
  switch (entry.mode) {
    case 'after':
      return { name, title, mode: 'after', ...(await shoot(scenario, head, capturedFile(name), failedFile(name))) };
    case 'before-after': {
      const baseShot: Shot = base
        ? await shoot(scenario, base, baseCapturedFile(name), baseFailedFile(name))
        : { status: 'failed', error: 'there is no base app: WISP_BASE_APP_BUNDLE is not set, or the base-build step failed' };
      const headShot = await shoot(scenario, head, capturedFile(name), failedFile(name));
      return { name, title, mode: 'before-after', ...headShot, base: baseShot };
    }
    case 'video':
      return { name, title, mode: 'video', ...(await record(scenario, head)) };
  }
}

interface Take {
  /** The scene's screenshot, or why it has none. */
  outcome: { kind: 'shot'; png: Buffer } | { kind: 'not-available'; reason: string } | { kind: 'failed'; error: string; png?: Buffer };
  /** The last window the scene used, which is the one a video shows. */
  video: Video | null;
}

/** Launches the app, runs the scene, and quits the app again. */
async function take(scenario: Scenario, options: LaunchOptions, hold = 0): Promise<Take> {
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
      return { outcome: { kind: 'not-available', reason: shot.notAvailable }, video: null };
    }
    await delay(hold);
    return { outcome: { kind: 'shot', png: shot }, video: latest?.window.video() ?? null };
  } catch (error) {
    const png = latest ? await screenshot(latest.window, failureShotTimeoutMs).catch(() => undefined) : undefined;
    return { outcome: { kind: 'failed', error: messageOf(error), ...(png ? { png } : {}) }, video: null };
  } finally {
    await session?.close();
  }
}

async function shoot(scenario: Scenario, options: LaunchOptions, captured: string, failed: string): Promise<Shot> {
  const { outcome } = await take(scenario, options);
  switch (outcome.kind) {
    case 'shot':
      await writeFile(join(outDir, captured), outcome.png);
      return { status: 'captured', file: captured };
    case 'not-available':
      return { status: 'not-available', reason: outcome.reason };
    case 'failed':
      if (outcome.png) {
        await writeFile(join(outDir, failed), outcome.png);
        return { status: 'failed', error: outcome.error, file: failed };
      }
      return { status: 'failed', error: outcome.error };
  }
}

/** Runs the scene with Playwright's recordVideo, keeps the window's WebM, and makes a GIF preview of it. */
async function record(scenario: Scenario, options: LaunchOptions): Promise<Shot & { video?: string }> {
  const { name } = scenario;
  const dir = await mkdtemp(join(tmpdir(), 'wisp-video-'));
  try {
    const { outcome, video } = await take(scenario, { ...options, recordVideo: { dir, size: WINDOW_SIZE } }, videoHoldMs);
    switch (outcome.kind) {
      case 'not-available':
        return { status: 'not-available', reason: outcome.reason };
      case 'failed':
        if (outcome.png) {
          await writeFile(join(outDir, failedFile(name)), outcome.png);
          return { status: 'failed', error: outcome.error, file: failedFile(name) };
        }
        return { status: 'failed', error: outcome.error };
      case 'shot':
        break;
    }
    try {
      if (!video) {
        throw new Error('Playwright recorded no video of the window');
      }
      const webm = join(outDir, videoFile(name));
      await withTimeout(video.saveAs(webm), videoSaveTimeoutMs);
      // publish rejects the whole artifact over one oversized file, so an oversized video fails only its scene.
      const { size } = await stat(webm);
      if (size > MAX_BYTES.webm) {
        await rm(webm, { force: true });
        throw new Error(`the video is ${String(size)} bytes, more than ${String(MAX_BYTES.webm)}; shorten the scene`);
      }
      await gif(webm, join(outDir, capturedFile(name, 'video')));
      return { status: 'captured', file: capturedFile(name, 'video'), video: videoFile(name) };
    } catch (error) {
      await rm(join(outDir, videoFile(name)), { force: true });
      await writeFile(join(outDir, failedFile(name)), outcome.png);
      return { status: 'failed', error: `the scene ran, but its video could not be saved: ${messageOf(error)}`, file: failedFile(name) };
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

async function gif(webm: string, out: string): Promise<void> {
  const ffmpeg = process.env.WISP_FFMPEG ?? 'ffmpeg';
  for (const { fps, width } of gifAttempts) {
    const filter = `fps=${String(fps)},scale=${String(width)}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle`;
    try {
      await execFileAsync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-i', webm, '-vf', filter, '-loop', '0', out], {
        timeout: 180_000,
      });
    } catch (error) {
      const missing = (error as NodeJS.ErrnoException).code === 'ENOENT';
      throw new Error(
        missing ? `${ffmpeg} is not installed; the workflow installs it with Homebrew when a video is requested` : `ffmpeg failed: ${messageOf(error)}`,
        { cause: error },
      );
    }
    const { size } = await stat(out);
    if (size <= maxGifBytes) {
      console.log(`${webm}: GIF preview at ${String(fps)} fps and ${String(width)} px is ${String(size)} bytes`);
      return;
    }
  }
  await rm(out, { force: true });
  throw new Error(`the GIF preview is larger than ${String(maxGifBytes)} bytes even at 5 fps and 480 px; shorten the scene`);
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

function describe(shot: Shot): string {
  switch (shot.status) {
    case 'captured':
      return `captured ${shot.file}`;
    case 'not-available':
      return `not available yet (${shot.reason})`;
    case 'failed':
      return `failed: ${shot.error}`;
    case 'pending':
      return 'did not run';
  }
}

function messageOf(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.length > LIMITS.error ? `${message.slice(0, LIMITS.error - 6)} [cut]` : message;
}
