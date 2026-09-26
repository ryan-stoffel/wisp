import { rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

export const MODES = ['after', 'before-after', 'video'] as const;
export type Mode = (typeof MODES)[number];

export type Shot =
  | { status: 'pending' }
  | { status: 'captured'; file: string }
  | { status: 'not-available'; reason: string }
  | { status: 'failed'; error: string; file?: string };

/**
 * One requested scene. The shot fields describe the head commit's app. `before-after` adds `base`, the
 * same scene against the base commit's app, and a captured `video` names its WebM next to the GIF in `file`.
 */
export type Result = { name: string; title: string; mode: Mode; base?: Shot; video?: string } & Shot;

export interface Manifest {
  error?: string;
  results: Result[];
}

export const MANIFEST_FILE = 'manifest.json';
export const LIMITS = { results: 50, name: 64, title: 100, reason: 300, error: 20_000 };

const namePattern = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const textPattern = /^[A-Za-z0-9 ,.:;'"()/+&=_-]+$/;

export function capturedFile(name: string, mode: Mode = 'after'): string {
  return mode === 'video' ? `${name}.gif` : `${name}.png`;
}

export function failedFile(name: string): string {
  return `${name}.failed.png`;
}

export function baseCapturedFile(name: string): string {
  return `${name}.base.png`;
}

export function baseFailedFile(name: string): string {
  return `${name}.base.failed.png`;
}

export function videoFile(name: string): string {
  return `${name}.webm`;
}

export function nameProblem(name: string): string | undefined {
  if (name.length > LIMITS.name || !namePattern.test(name)) {
    return `must be lowercase letters and digits joined by single hyphens, at most ${String(LIMITS.name)} characters`;
  }
  return undefined;
}

export function textProblem(text: string, max: number): string | undefined {
  if (text.length > max || !textPattern.test(text) || text.trim() !== text) {
    return `must be one line of at most ${String(max)} letters, digits, spaces, and , . : ; ' " ( ) / + & = _ -`;
  }
  if (text.includes('://') || /:[\w+-]+:/.test(text)) {
    return 'must not contain a URL or an emoji code';
  }
  return undefined;
}

export function parseManifest(value: unknown): Manifest {
  if (!isRecord(value) || !Array.isArray(value.results)) {
    throw new Error('manifest.json is not an object with a results array');
  }
  if (value.results.length > LIMITS.results) {
    throw new Error(`manifest.json has more than ${String(LIMITS.results)} results`);
  }
  const names = new Set<string>();
  const results = value.results.map((entry: unknown, index) => parseResult(entry, `results[${String(index)}]`, names));
  const { error } = value;
  if (error === undefined) {
    return { results };
  }
  if (typeof error !== 'string' || error.length > LIMITS.error) {
    throw new Error(`manifest.json error is not a string of at most ${String(LIMITS.error)} characters`);
  }
  return { error, results };
}

function parseResult(entry: unknown, where: string, names: Set<string>): Result {
  if (!isRecord(entry)) {
    throw new Error(`${where} is not an object`);
  }
  const { name, title, mode } = entry;
  if (typeof name !== 'string') {
    throw new Error(`${where}.name is not a string`);
  }
  const badName = nameProblem(name);
  if (badName) {
    throw new Error(`${where}.name ${badName}`);
  }
  if (names.has(name)) {
    throw new Error(`${where}.name ${name} appears twice`);
  }
  names.add(name);
  const badTitle = typeof title === 'string' ? textProblem(title, LIMITS.title) : 'is not a string';
  if (badTitle || typeof title !== 'string') {
    throw new Error(`${where}.title ${badTitle ?? ''}`);
  }
  if (!MODES.includes(mode as Mode)) {
    throw new Error(`${where}.mode is not ${MODES.join(', ')}`);
  }
  const resultMode = mode as Mode;

  const shot = parseShot(entry, where, { captured: capturedFile(name, resultMode), failed: failedFile(name) });
  const result: Result = { name, title, mode: resultMode, ...shot };
  if (resultMode === 'before-after') {
    result.base = parseShot(entry.base, `${where}.base`, { captured: baseCapturedFile(name), failed: baseFailedFile(name) });
  } else if (entry.base !== undefined) {
    throw new Error(`${where}.base is only for before-after`);
  }
  if (resultMode === 'video' && shot.status === 'captured') {
    if (entry.video !== videoFile(name)) {
      throw new Error(`${where}.video must be ${videoFile(name)}`);
    }
    result.video = videoFile(name);
  } else if (entry.video !== undefined) {
    throw new Error(`${where}.video is only for a captured video`);
  }
  return result;
}

function parseShot(entry: unknown, where: string, files: { captured: string; failed: string }): Shot {
  if (!isRecord(entry)) {
    throw new Error(`${where} is not an object`);
  }
  const { status } = entry;
  switch (status) {
    case 'pending':
      return { status };
    case 'captured':
      if (entry.file !== files.captured) {
        throw new Error(`${where}.file must be ${files.captured}`);
      }
      return { status, file: files.captured };
    case 'not-available': {
      const { reason } = entry;
      const badReason = typeof reason === 'string' ? textProblem(reason, LIMITS.reason) : 'is not a string';
      if (badReason || typeof reason !== 'string') {
        throw new Error(`${where}.reason ${badReason ?? ''}`);
      }
      return { status, reason };
    }
    case 'failed': {
      const { error, file } = entry;
      if (typeof error !== 'string' || error.length > LIMITS.error) {
        throw new Error(`${where}.error is not a string of at most ${String(LIMITS.error)} characters`);
      }
      if (file === undefined) {
        return { status, error };
      }
      if (file !== files.failed) {
        throw new Error(`${where}.file must be ${files.failed}`);
      }
      return { status, error, file };
    }
    default:
      throw new Error(`${where}.status is not pending, captured, not-available, or failed`);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export async function writeManifest(dir: string, manifest: Manifest): Promise<void> {
  const path = join(dir, MANIFEST_FILE);
  await writeFile(`${path}.tmp`, `${JSON.stringify(manifest, null, 2)}\n`);
  await rename(`${path}.tmp`, path);
}
