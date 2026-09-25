// Reads wisp's exclusion id lists straight from the overlay source that ships them, instead of hardcoding a
// copy that could drift. scripts/ci/check-fork's own sed extractor only matches a single-quoted id followed
// by a trailing comma (#94's finding 1); this parser accepts either quote style, a missing trailing comma, and
// a trailing line comment, so an id that would silently vanish from check-fork's guard still shows up here.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const repoRoot = join(import.meta.dirname, '..', '..', '..');

const viewContainersFile = join(
  repoRoot,
  'editor/overlay/src/vs/workbench/common/wisp/exclusions.ts',
);
const actionsFile = join(
  repoRoot,
  'editor/overlay/src/vs/platform/wisp/common/excludedActions.ts',
);

export interface ExclusionLists {
  readonly viewContainers: readonly string[];
  readonly workbenchContributions: readonly string[];
  readonly actions: readonly string[];
}

export async function readExclusionLists(): Promise<ExclusionLists> {
  const [exclusions, actionsSource] = await Promise.all([
    readFile(viewContainersFile, 'utf8'),
    readFile(actionsFile, 'utf8'),
  ]);
  return {
    viewContainers: parseIdArray(exclusions, 'excludedViewContainers'),
    workbenchContributions: parseIdArray(exclusions, 'excludedWorkbenchContributions'),
    actions: parseIdArray(actionsSource, 'excludedActions'),
  };
}

function parseIdArray(source: string, constName: string): string[] {
  const declaration = new RegExp(`\\b${constName}\\b[^=]*=[^\\[]*\\[`).exec(source);
  if (!declaration) {
    throw new Error(`could not find a "${constName} = ... [" declaration`);
  }
  const open = declaration.index + declaration[0].length - 1;
  const close = matchingBracket(source, open);
  const ids: string[] = [];
  for (const rawLine of source.slice(open + 1, close).split('\n')) {
    const line = stripLineComment(rawLine).trim();
    if (line === '') {
      continue;
    }
    const entry = /^(['"])((?:(?!\1).)+)\1\s*,?$/.exec(line);
    const id = entry?.[2];
    if (id === undefined) {
      throw new Error(`${constName}: line does not look like a quoted id or a comment: ${JSON.stringify(rawLine)}`);
    }
    ids.push(id);
  }
  return ids;
}

function stripLineComment(line: string): string {
  return line.replace(/\/\/.*$/, '');
}

function matchingBracket(source: string, openIndex: number): number {
  let depth = 0;
  for (let i = openIndex; i < source.length; i++) {
    if (source[i] === '[') depth++;
    else if (source[i] === ']') {
      depth--;
      if (depth === 0) return i;
    }
  }
  throw new Error('unterminated array');
}
