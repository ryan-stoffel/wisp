import { lstat, open } from 'node:fs/promises';
import { join } from 'node:path';
import { MANIFEST_FILE, parseManifest, type Manifest } from './manifest.ts';

export interface Capture {
  manifest: Manifest;
  files: string[];
}

const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const maxManifestBytes = 1024 * 1024;
const maxPngBytes = 10 * 1024 * 1024;
const maxTotalBytes = 25 * 1024 * 1024;
const logTailBytes = 64 * 1024;

export async function readCapture(dir: string): Promise<Capture | undefined> {
  const dirKind = await kind(dir);
  if (dirKind === 'missing') {
    return undefined;
  }
  if (dirKind !== 'directory') {
    throw new Error(`${dir} is not a directory`);
  }
  const manifestPath = join(dir, MANIFEST_FILE);
  const manifestKind = await kind(manifestPath);
  if (manifestKind === 'missing') {
    return undefined;
  }
  if (typeof manifestKind !== 'number') {
    throw new Error(`${MANIFEST_FILE} is not a regular file`);
  }
  if (manifestKind > maxManifestBytes) {
    throw new Error(`${MANIFEST_FILE} is larger than ${String(maxManifestBytes)} bytes`);
  }
  let json: unknown;
  try {
    json = JSON.parse((await readBytes(manifestPath, 0, manifestKind)).toString('utf8'));
  } catch {
    throw new Error(`${MANIFEST_FILE} is not valid JSON`);
  }
  const manifest = parseManifest(json);

  const files: string[] = [];
  let totalBytes = 0;
  for (const result of manifest.results) {
    if (!('file' in result)) {
      continue;
    }
    const path = join(dir, result.file);
    const size = await kind(path);
    if (typeof size !== 'number') {
      throw new Error(`${result.file} is listed in ${MANIFEST_FILE} but is not a regular file`);
    }
    if (size > maxPngBytes) {
      throw new Error(`${result.file} is larger than ${String(maxPngBytes)} bytes`);
    }
    totalBytes += size;
    if (totalBytes > maxTotalBytes) {
      throw new Error(`the capture totals more than ${String(maxTotalBytes)} bytes`);
    }
    if (!(await readBytes(path, 0, pngSignature.length)).equals(pngSignature)) {
      throw new Error(`${result.file} is not a PNG`);
    }
    files.push(result.file);
  }
  return { manifest, files };
}

export function missingArtifactError(capture: Capture | undefined, captureOutcome: string | undefined): string | undefined {
  if (capture !== undefined || captureOutcome !== 'success') {
    return undefined;
  }
  return 'the capture job succeeded, but its results were not downloaded';
}

export async function readLogTail(dir: string, file: string): Promise<string | undefined> {
  if ((await kind(dir)) !== 'directory') {
    return undefined;
  }
  const path = join(dir, file);
  const size = await kind(path);
  if (typeof size !== 'number') {
    return undefined;
  }
  const start = Math.max(0, size - logTailBytes);
  return (await readBytes(path, start, size - start)).toString('utf8');
}

async function kind(path: string): Promise<number | 'directory' | 'missing' | 'other'> {
  try {
    const stats = await lstat(path);
    if (stats.isFile()) {
      return stats.size;
    }
    return stats.isDirectory() ? 'directory' : 'other';
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
      return 'missing';
    }
    throw error;
  }
}

async function readBytes(path: string, position: number, length: number): Promise<Buffer> {
  const handle = await open(path, 'r');
  try {
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, position);
    return buffer.subarray(0, bytesRead);
  } finally {
    await handle.close();
  }
}
