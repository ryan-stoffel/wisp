import { lstat, open } from 'node:fs/promises';
import { join } from 'node:path';
import { MANIFEST_FILE, parseManifest, type Manifest, type Result } from './manifest.ts';

export interface Capture {
  manifest: Manifest;
  files: string[];
}

const maxManifestBytes = 1024 * 1024;
const megabyte = 1024 * 1024;
/** What each kind of file must start with, and how large it may be. */
const kinds = {
  png: { signatures: [Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])], maxBytes: 10 * megabyte },
  gif: { signatures: [Buffer.from('GIF87a'), Buffer.from('GIF89a')], maxBytes: 10 * megabyte },
  webm: { signatures: [Buffer.from([0x1a, 0x45, 0xdf, 0xa3])], maxBytes: 25 * megabyte },
} as const;
const maxTotalBytes = 60 * megabyte;
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
  for (const file of manifest.results.flatMap(filesOf)) {
    const extension = file.slice(file.lastIndexOf('.') + 1);
    if (!(extension in kinds)) {
      throw new Error(`${file} is not a png, gif, or webm file`);
    }
    const { signatures, maxBytes } = kinds[extension as keyof typeof kinds];
    const path = join(dir, file);
    const size = await kind(path);
    if (typeof size !== 'number') {
      throw new Error(`${file} is listed in ${MANIFEST_FILE} but is not a regular file`);
    }
    if (size > maxBytes) {
      throw new Error(`${file} is larger than ${String(maxBytes)} bytes`);
    }
    totalBytes += size;
    if (totalBytes > maxTotalBytes) {
      throw new Error(`the capture totals more than ${String(maxTotalBytes)} bytes`);
    }
    const head = await readBytes(path, 0, 8);
    if (!signatures.some((signature) => head.subarray(0, signature.length).equals(signature))) {
      throw new Error(`${file} is not a ${extension.toUpperCase()}`);
    }
    files.push(file);
  }
  return { manifest, files };
}

function filesOf(result: Result): string[] {
  return [result, ...(result.base ? [result.base] : [])]
    .flatMap((shot) => ('file' in shot ? [shot.file] : []))
    .concat(result.video === undefined ? [] : [result.video]);
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
