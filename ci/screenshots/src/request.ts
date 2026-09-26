import { MODES, nameProblem, type Mode } from './manifest.ts';
import { linesOutsideFences, withoutSection } from './section.ts';

/** The label that turns screenshots.yml on for a pull request (0018). */
export const LABEL = 'screenshots';
export const MAX_SCENES = 10;

export interface Entry {
  mode: Mode;
  name: string;
}

export interface Request {
  entries: Entry[];
}

/**
 * A problem with a request, worded for the pull request's author. The message is built only from
 * fixed text and names that passed `nameProblem`, never from the body's raw text.
 */
export class RequestError extends Error {}

const openPattern = /^[ \t]*<!--[ \t]*wisp-media[ \t]*$/;
const linePattern = /^([a-z-]+)[ \t]*:[ \t]*(.*)$/;

/**
 * The text of the first `<!-- wisp-media` block, if any. The opening must be a line of its own, outside
 * fenced code and outside the section the workflow writes, so quoting a block does not request it.
 */
export function requestBlock(body: string | null | undefined): string | undefined {
  const text = withoutSection(body ?? '');
  const open = linesOutsideFences(text).find((line) => openPattern.test(line.text));
  if (!open) {
    return undefined;
  }
  const rest = text.slice(open.end).replace(/^\r?\n/, '');
  const close = rest.indexOf('-->');
  return close === -1 ? undefined : rest.slice(0, close);
}

export function parseBody(body: string | null | undefined): Request {
  const block = requestBlock(body);
  if (block === undefined) {
    throw new RequestError(
      `The pull request has the ${LABEL} label, but its body has no wisp-media block, so there is nothing to capture. Add the block (see Visuals in the pull request template) or remove the label.`,
    );
  }
  return parseRequest(block);
}

/** Parses `mode: scene, scene` lines; `;` also ends a line, and blank lines and `#` comments are skipped. */
export function parseRequest(text: string): Request {
  const entries: Entry[] = [];
  const seen = new Set<string>();
  for (const [index, rawLine] of text.split('\n').entries()) {
    const where = `Line ${String(index + 1)} of the wisp-media block`;
    for (const part of rawLine.split(';')) {
      const line = part.trim();
      if (line === '' || line.startsWith('#')) {
        continue;
      }
      const match = linePattern.exec(line);
      if (!match) {
        throw new RequestError(`${where} is not a mode, a colon, and scene names, as in after: agents-window.`);
      }
      const [, mode = '', list = ''] = match;
      if (!MODES.includes(mode as Mode)) {
        throw new RequestError(`${where} has an unknown mode${nameProblem(mode) ? '' : ` (${mode})`}. Use ${MODES.join(', ')}.`);
      }
      const names = list.split(/[\s,]+/).filter((name) => name !== '');
      if (names.length === 0) {
        throw new RequestError(`${where} names no scene.`);
      }
      for (const name of names) {
        if (nameProblem(name)) {
          throw new RequestError(`${where} has a scene name that is not lowercase letters and digits joined by hyphens.`);
        }
        if (seen.has(name)) {
          throw new RequestError(`The wisp-media block names ${name} twice. Name each scene once; before-after already includes the after.`);
        }
        seen.add(name);
        entries.push({ mode: mode as Mode, name });
      }
    }
  }
  if (entries.length === 0) {
    throw new RequestError('The wisp-media block names no scene.');
  }
  if (entries.length > MAX_SCENES) {
    throw new RequestError(`The wisp-media block names ${String(entries.length)} scenes; the limit is ${String(MAX_SCENES)}.`);
  }
  return { entries };
}

export function checkScenes(request: Request, known: readonly string[]): void {
  const unknown = request.entries.map((entry) => entry.name).filter((name) => !known.includes(name));
  if (unknown.length > 0) {
    throw new RequestError(
      `Unknown scene${unknown.length > 1 ? 's' : ''}: ${unknown.join(', ')}. Scenes are the names in ci/screenshots/src/scenarios.ts: ${known.join(', ')}.`,
    );
  }
}

/** The request as one line, `after: a, b; video: c`, which parseRequest reads back. */
export function formatRequest(request: Request): string {
  return MODES.flatMap((mode) => {
    const names = request.entries.filter((entry) => entry.mode === mode).map((entry) => entry.name);
    return names.length > 0 ? [`${mode}: ${names.join(', ')}`] : [];
  }).join('; ');
}

export function wants(request: Request, mode: Mode): boolean {
  return request.entries.some((entry) => entry.mode === mode);
}

export interface Pull {
  state: string;
  headRepo: string | undefined;
  labels: readonly string[];
  body: string | null;
}

export interface PullEvent {
  name: string;
  action?: string | undefined;
  label?: string | undefined;
  changes?: { body?: { from?: unknown }; base?: unknown } | undefined;
}

export type Decision =
  /** Nothing to do, and nothing to report in the body. */
  | { kind: 'skip'; reason: string }
  /** The run cannot go ahead. `publish` says whether the problem belongs in the body's section. */
  | { kind: 'problem'; problem: string; publish: boolean }
  | { kind: 'run'; request: Request };

export function decide(event: PullEvent, pull: Pull, repository: string, knownScenes: readonly string[]): Decision {
  if (pull.headRepo !== repository) {
    return {
      kind: 'skip',
      reason: `The head branch is in ${pull.headRepo ?? 'a deleted fork'}, not ${repository}. Pull requests from forks get a read-only token, so this workflow cannot push to ci-screenshots or edit the pull request.`,
    };
  }
  if (!pull.labels.includes(LABEL)) {
    return event.name === 'workflow_dispatch'
      ? { kind: 'problem', problem: `The pull request does not have the ${LABEL} label, and nothing is captured without it.`, publish: false }
      : { kind: 'skip', reason: `The pull request no longer has the ${LABEL} label.` };
  }
  if (pull.state !== 'open') {
    return { kind: 'problem', problem: 'The pull request is not open.', publish: false };
  }
  if (event.name === 'pull_request' && event.action === 'labeled' && event.label !== LABEL) {
    return { kind: 'skip', reason: `The label added was not ${LABEL}.` };
  }
  if (event.name === 'pull_request' && event.action === 'edited' && event.changes?.base === undefined) {
    const from = event.changes?.body?.from;
    if (typeof from !== 'string' || requestBlock(from) === requestBlock(pull.body)) {
      return { kind: 'skip', reason: 'The edit changed neither the wisp-media block nor the base branch.' };
    }
  }
  try {
    const request = parseBody(pull.body);
    checkScenes(request, knownScenes);
    return { kind: 'run', request };
  } catch (error) {
    if (error instanceof RequestError) {
      return { kind: 'problem', problem: error.message, publish: true };
    }
    throw error;
  }
}
