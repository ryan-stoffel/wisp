import { rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

interface Base {
  name: string;
  title: string;
}

export type Result = Base &
  (
    | { status: 'pending' }
    | { status: 'captured'; file: string }
    | { status: 'failed'; error: string; file?: string }
  );

export interface Manifest {
  error?: string;
  results: Result[];
}

export const MANIFEST_FILE = 'manifest.json';
export const LIMITS = { results: 50, name: 64, title: 100, error: 20_000 };

const namePattern = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const textPattern = /^[A-Za-z0-9 ,.:;'"()/+&=_-]+$/;

export function capturedFile(name: string): string {
  return `${name}.png`;
}

export function failedFile(name: string): string {
  return `${name}.failed.png`;
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
  const { name, title, status } = entry;
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

  switch (status) {
    case 'pending':
      return { name, title, status };
    case 'captured':
      if (entry.file !== capturedFile(name)) {
        throw new Error(`${where}.file must be ${capturedFile(name)}`);
      }
      return { name, title, status, file: capturedFile(name) };
    case 'failed': {
      const { error, file } = entry;
      if (typeof error !== 'string' || error.length > LIMITS.error) {
        throw new Error(`${where}.error is not a string of at most ${String(LIMITS.error)} characters`);
      }
      if (file === undefined) {
        return { name, title, status, error };
      }
      if (file !== failedFile(name)) {
        throw new Error(`${where}.file must be ${failedFile(name)}`);
      }
      return { name, title, status, error, file };
    }
    default:
      throw new Error(`${where}.status is not pending, captured, or failed`);
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
