import { readFile, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

interface Base {
  name: string;
  title: string;
}

export type Result = Base &
  (
    | { status: 'pending' }
    | { status: 'captured'; file: string }
    | { status: 'not-available'; reason: string }
    | { status: 'failed'; error: string; file?: string }
  );

export interface Manifest {
  error?: string;
  results: Result[];
}

export const MANIFEST_FILE = 'manifest.json';

export async function writeManifest(dir: string, manifest: Manifest): Promise<void> {
  const path = join(dir, MANIFEST_FILE);
  await writeFile(`${path}.tmp`, `${JSON.stringify(manifest, null, 2)}\n`);
  await rename(`${path}.tmp`, path);
}

export async function readManifest(dir: string): Promise<Manifest | undefined> {
  try {
    return JSON.parse(await readFile(join(dir, MANIFEST_FILE), 'utf8')) as Manifest;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
      return undefined;
    }
    throw error;
  }
}
