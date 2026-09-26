import { stripVTControlCharacters } from 'node:util';
import type { Manifest, Result, Shot } from './manifest.ts';

export const START = '<!-- wisp-media:start -->';
export const END = '<!-- wisp-media:end -->';

export interface FailedStep {
  id: string;
  log?: string;
}

export interface Images {
  /** raw.githubusercontent.com folder, pinned to a ci-screenshots commit, that the files are embedded from. */
  raw: string;
  /** github.com folder view of the same files, for links. */
  blob: string;
  tree: string;
}

export interface SectionInput {
  serverUrl: string;
  repository: string;
  prNumber: number;
  headSha: string;
  baseSha?: string;
  runUrl: string;
  manifest: Manifest | undefined;
  images: Images | undefined;
  requestProblem?: string;
  pushError?: string;
  artifactError?: string;
  failedSteps: readonly FailedStep[];
}

const logLines = 40;
const maxLineLength = 300;
const maxErrorLength = 3_000;
const maxCellLength = 200;

/** The section's content, markers included, ready for replaceSection. */
export function renderSection(input: SectionInput): string {
  const { manifest } = input;
  const results = manifest?.results ?? [];
  const blocks = [
    '## Screenshots',
    `${input.requestProblem === undefined ? 'Captured from' : 'Checked'} ${commitLink(input, input.headSha)} by [this run](${input.runUrl}) for the wisp-media block in this description${
      results.length > 0 ? `: ${tally(results)}` : ''
    }.`,
  ];

  const problem = problemOf(input);
  if (problem) {
    blocks.push(`> [!CAUTION]\n> ${problem}`);
  }
  if (input.artifactError !== undefined && hasMoreLines(input.artifactError)) {
    blocks.push(details('Why the results were rejected', fence(clip(input.artifactError))));
  }
  const incomplete = !manifest || manifest.error !== undefined || results.some((result) => result.status === 'pending');
  for (const step of input.failedSteps) {
    if (step.log && (incomplete || step.id === 'base-build')) {
      blocks.push(details(`Last lines of the ${step.id} step's log`, fence(tail(step.log))));
    }
  }
  if (manifest?.error !== undefined && hasMoreLines(manifest.error)) {
    blocks.push(details('Capture error', fence(clip(manifest.error))));
  }
  if (input.pushError !== undefined) {
    blocks.push(details('Push error', fence(clip(input.pushError))));
  }

  for (const result of results) {
    blocks.push(`### ${result.title}`, ...section(result, input));
  }

  if (input.images && results.some((result) => 'file' in result || result.base?.status === 'captured')) {
    blocks.push(`<sub>The files are on the [ci-screenshots branch](${input.images.tree}).</sub>`);
  }
  // Untrusted text never forges a marker or a request block, which both start with this.
  const body = blocks.join('\n\n').replaceAll('<!--', '&lt;!--');
  return `${START}\n${body}\n${END}`;
}

/**
 * Replaces the text from START through END with `section`, or appends it when the body has no START.
 * A START with no END is a section someone cut short, so everything after it is replaced.
 */
export function replaceSection(body: string | null | undefined, section: string): string {
  const text = body ?? '';
  const span = sectionSpan(text);
  if (!span) {
    const kept = text.trimEnd();
    return kept === '' ? `${section}\n` : `${kept}\n\n${section}\n`;
  }
  const after = text.slice(span.end);
  return `${text.slice(0, span.start)}${section}${after === '' ? '\n' : after}`;
}

/** The body with the section the workflow writes taken out, so nothing inside it counts as a request. */
export function withoutSection(body: string): string {
  const span = sectionSpan(body);
  return span ? `${body.slice(0, span.start)}${body.slice(span.end)}` : body;
}

export interface Line {
  /** Offset of the line's first character. */
  start: number;
  /** Offset just past the line's text, before its line ending. */
  end: number;
  text: string;
}

/**
 * The body's lines that are outside fenced code blocks. A marker or a request block counts only as a
 * line of its own there, so a description can quote either in inline code or in a fence.
 */
export function linesOutsideFences(body: string): Line[] {
  const lines: Line[] = [];
  let fence: { char: string; length: number } | undefined;
  let start = 0;
  while (start <= body.length) {
    const newline = body.indexOf('\n', start);
    const next = newline === -1 ? body.length + 1 : newline + 1;
    const end = newline === -1 ? body.length : newline > start && body[newline - 1] === '\r' ? newline - 1 : newline;
    const text = body.slice(start, end);
    const marker = /^ {0,3}(`{3,}|~{3,})/.exec(text)?.[1];
    if (fence) {
      if (marker?.startsWith(fence.char) && marker.length >= fence.length && text.trim() === marker) {
        fence = undefined;
      }
    } else if (marker) {
      fence = { char: marker.charAt(0), length: marker.length };
    } else {
      lines.push({ start, end, text });
    }
    start = next;
  }
  return lines;
}

function sectionSpan(body: string): { start: number; end: number } | undefined {
  const lines = linesOutsideFences(body);
  const startLine = lines.find((line) => line.text.trim() === START);
  if (!startLine) {
    return undefined;
  }
  const endLine = lines.find((line) => line.start > startLine.start && line.text.trim() === END);
  return { start: startLine.start, end: endLine ? endLine.end : body.length };
}

function commitLink(input: SectionInput, sha: string): string {
  const path = sha === input.headSha ? `pull/${String(input.prNumber)}/commits` : 'commit';
  return `[${code(sha.slice(0, 7))}](${input.serverUrl}/${input.repository}/${path}/${sha})`;
}

function problemOf(input: SectionInput): string | undefined {
  const { manifest } = input;
  if (input.requestProblem !== undefined) {
    return `Nothing captured: ${input.requestProblem}`;
  }
  if (input.artifactError !== undefined) {
    return `Nothing captured: the capture job's results were rejected: ${code(firstLine(input.artifactError))}.`;
  }
  if (!manifest) {
    const steps = input.failedSteps.map((step) => code(step.id)).join(', ');
    return steps ? `Nothing captured: the ${steps} step failed.` : 'Nothing captured: capture did not run.';
  }
  if (manifest.error !== undefined) {
    return `Capture stopped early: ${code(firstLine(manifest.error))}.`;
  }
  const total = String(manifest.results.length);
  const failed = manifest.results.filter((result) => result.status === 'failed').length;
  if (failed > 0) {
    return `${String(failed)} of ${total} scenes failed.`;
  }
  const pending = manifest.results.filter((result) => result.status === 'pending').length;
  if (pending > 0) {
    return `Capture stopped before ${String(pending)} of ${total} scenes ran.`;
  }
  if (input.pushError !== undefined) {
    return 'The scenes were captured, but pushing them to the ci-screenshots branch failed.';
  }
  return undefined;
}

function section(result: Result, input: SectionInput): string[] {
  switch (result.mode) {
    case 'after':
      return shotBlocks(result.title, result, input.images);
    case 'video':
      return videoBlocks(result, input.images);
    case 'before-after': {
      const base = input.baseSha ? commitLink(input, input.baseSha) : 'the base commit';
      return [
        `| Before: ${base} | After: ${commitLink(input, input.headSha)} |\n| --- | --- |\n| ${cell(`${result.title}, before`, result.base ?? { status: 'pending' }, input.images)} | ${cell(`${result.title}, after`, result, input.images)} |`,
        ...(result.base?.status === 'failed' && hasMoreLines(result.base.error) ? [details('Error on the base app', fence(clip(result.base.error)))] : []),
        ...(result.status === 'failed' && hasMoreLines(result.error) ? [details('Error', fence(clip(result.error)))] : []),
      ];
    }
  }
}

function shotBlocks(alt: string, shot: Shot, images: Images | undefined): string[] {
  switch (shot.status) {
    case 'captured':
      return [image(alt, shot.file, images)];
    case 'not-available':
      return [`Not available yet: ${shot.reason}.`];
    case 'failed':
      return [
        `Failed: ${code(firstLine(shot.error))}`,
        ...(hasMoreLines(shot.error) ? [details('Error', fence(clip(shot.error)))] : []),
        ...(shot.file ? [image(`${alt} when it failed`, shot.file, images)] : []),
      ];
    case 'pending':
      return ['Did not run: capture stopped before this scene.'];
  }
}

function videoBlocks(result: Result, images: Images | undefined): string[] {
  if (result.status !== 'captured' || result.video === undefined) {
    return shotBlocks(result.title, result, images);
  }
  if (!images) {
    return ['Recorded, but the video was not pushed.'];
  }
  const video = `${images.blob}/${result.video}`;
  return [`[![${result.title}](${images.raw}/${result.file})](${video})`, `A preview; [the full video](${video}) is a WebM.`];
}

/** One side of a before and after table: a single line, with pipes escaped. */
function cell(alt: string, shot: Shot, images: Images | undefined): string {
  let text: string;
  switch (shot.status) {
    case 'captured':
      text = image(alt, shot.file, images);
      break;
    case 'not-available':
      text = `Not available: ${shot.reason}.`;
      break;
    case 'failed': {
      const reason = `Failed: ${code(clipLine(firstLine(shot.error)))}`;
      text = shot.file ? `${reason}<br>${image(`${alt} when it failed`, shot.file, images)}` : reason;
      break;
    }
    case 'pending':
      text = 'Did not run.';
      break;
  }
  return text.replaceAll('|', '\\|');
}

function image(alt: string, file: string, images: Images | undefined): string {
  return images ? `![${alt}](${images.raw}/${file})` : `Captured, but the file was not pushed.`;
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

function clipLine(text: string): string {
  return text.length > maxCellLength ? `${text.slice(0, maxCellLength)} [cut]` : text;
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
