import { stripVTControlCharacters } from 'node:util';
import type { Manifest, Result } from './manifest.ts';

export const MARKER = '<!-- wisp-screenshots -->';

export interface FailedStep {
  id: string;
  log?: string;
}

export interface Images {
  base: string;
  tree: string;
}

export interface CommentInput {
  serverUrl: string;
  repository: string;
  prNumber: number;
  headSha: string;
  runUrl: string;
  manifest: Manifest | undefined;
  images: Images | undefined;
  pushError?: string;
  artifactError?: string;
  failedSteps: readonly FailedStep[];
}

const logLines = 40;
const maxLineLength = 300;
const maxErrorLength = 3_000;

export function renderComment(input: CommentInput): string {
  const { manifest } = input;
  const short = input.headSha.slice(0, 7);
  const commit = `${input.serverUrl}/${input.repository}/pull/${String(input.prNumber)}/commits/${input.headSha}`;
  const results = manifest?.results ?? [];
  const blocks = [
    `${MARKER}\n## Screenshots`,
    `Screenshots of [${code(short)}](${commit}) from [this run](${input.runUrl})${results.length > 0 ? `: ${tally(results)}` : ''}.`,
  ];

  const problem = problemOf(input);
  if (problem) {
    blocks.push(`> [!CAUTION]\n> ${problem}`);
  }
  if (input.artifactError !== undefined && hasMoreLines(input.artifactError)) {
    blocks.push(details('Why the results were rejected', fence(clip(input.artifactError))));
  }
  if (!manifest || manifest.error !== undefined || results.some((result) => result.status === 'pending')) {
    for (const step of input.failedSteps) {
      if (step.log) {
        blocks.push(details(`Last lines of the ${step.id} step's log`, fence(tail(step.log))));
      }
    }
  }
  if (manifest?.error !== undefined && hasMoreLines(manifest.error)) {
    blocks.push(details('Capture error', fence(clip(manifest.error))));
  }
  if (input.pushError !== undefined) {
    blocks.push(details('Push error', fence(clip(input.pushError))));
  }

  for (const result of results) {
    blocks.push(`### ${result.title}`, ...section(result, input.images));
  }

  if (input.images && results.some((result) => 'file' in result)) {
    blocks.push(`<sub>The images are on the [ci-screenshots branch](${input.images.tree}).</sub>`);
  }
  return `${blocks.join('\n\n')}\n`;
}

function problemOf(input: CommentInput): string | undefined {
  const { manifest } = input;
  if (input.artifactError !== undefined) {
    return `No screenshots: the capture job's results were rejected: ${code(firstLine(input.artifactError))}.`;
  }
  if (!manifest) {
    const steps = input.failedSteps.map((step) => code(step.id)).join(', ');
    return steps ? `No screenshots: the ${steps} step failed.` : 'No screenshots: capture did not run.';
  }
  if (manifest.error !== undefined) {
    return `Capture stopped early: ${code(firstLine(manifest.error))}.`;
  }
  const total = String(manifest.results.length);
  const failed = manifest.results.filter((result) => result.status === 'failed').length;
  if (failed > 0) {
    return `${String(failed)} of ${total} scenarios failed.`;
  }
  const pending = manifest.results.filter((result) => result.status === 'pending').length;
  if (pending > 0) {
    return `Capture stopped before ${String(pending)} of ${total} scenarios ran.`;
  }
  if (input.pushError !== undefined) {
    return 'The screenshots were captured, but pushing them to the ci-screenshots branch failed.';
  }
  return undefined;
}

function section(result: Result, images: Images | undefined): string[] {
  switch (result.status) {
    case 'captured':
      return [image(result.title, result.file, images)];
    case 'not-available':
      return [`Not available yet: ${result.reason}.`];
    case 'failed':
      return [
        `Failed: ${code(firstLine(result.error))}`,
        ...(hasMoreLines(result.error) ? [details('Error', fence(clip(result.error)))] : []),
        ...(result.file ? [image(`${result.title} when it failed`, result.file, images)] : []),
      ];
    case 'pending':
      return ['Did not run: capture stopped before this scenario.'];
  }
}

function image(alt: string, file: string, images: Images | undefined): string {
  return images ? `![${alt}](${images.base}/${file})` : `Captured, but the image was not pushed.`;
}

function tally(results: readonly Result[]): string {
  const labels: Record<Result['status'], string> = {
    captured: 'captured',
    failed: 'failed',
    'not-available': 'not available yet',
    pending: 'did not run',
  };
  const counts = new Map<Result['status'], number>();
  for (const result of results) {
    counts.set(result.status, (counts.get(result.status) ?? 0) + 1);
  }
  return (Object.keys(labels) as Result['status'][])
    .filter((status) => counts.has(status))
    .map((status) => `${String(counts.get(status))} ${labels[status]}`)
    .join(', ');
}

function details(summary: string, body: string): string {
  return `<details><summary>${summary}</summary>\n\n${body}\n\n</details>`;
}

export function code(text: string): string {
  const fenceLength = longestBacktickRun(text) + 1;
  const ticks = '`'.repeat(fenceLength);
  const pad = text.startsWith('`') || text.endsWith('`') ? ' ' : '';
  return `${ticks}${pad}${text}${pad}${ticks}`;
}

export function fence(text: string): string {
  const ticks = '`'.repeat(Math.max(3, longestBacktickRun(text) + 1));
  return `${ticks}text\n${text}\n${ticks}`;
}

function longestBacktickRun(text: string): number {
  return Math.max(0, ...Array.from(text.matchAll(/`+/g), (match) => match[0].length));
}

function tail(log: string): string {
  return plain(log)
    .trimEnd()
    .split('\n')
    .slice(-logLines)
    .map((line) => (line.length > maxLineLength ? `${line.slice(0, maxLineLength)} [cut]` : line))
    .join('\n');
}

function clip(text: string): string {
  const clean = plain(text).trim();
  return clean.length > maxErrorLength ? `${clean.slice(0, maxErrorLength)}\n[cut]` : clean;
}

function firstLine(text: string): string {
  return plain(text).trim().split('\n')[0] ?? '';
}

function hasMoreLines(text: string): boolean {
  return plain(text).trim().includes('\n');
}

function plain(text: string): string {
  return stripVTControlCharacters(text).replace(/\r/g, '');
}
