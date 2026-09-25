import { execFile } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';

export interface BranchFile {
  path: string;
  source: string;
}

export interface CommitFilesOptions {
  remote: string;
  branch: string;
  files: readonly BranchFile[];
  message: string;
  env?: NodeJS.ProcessEnv;
  attempts?: number;
  retryDelayMs?: number;
  beforePush?: (attempt: number) => Promise<void>;
  log?: (line: string) => void;
}

const execFileAsync = promisify(execFile);

export async function commitFiles(options: CommitFilesOptions): Promise<string> {
  const { remote, branch, files, message, beforePush } = options;
  const env = options.env ?? process.env;
  const attempts = options.attempts ?? 5;
  const retryDelayMs = options.retryDelayMs ?? 2_000;
  const log = options.log ?? console.log;
  const ref = `refs/heads/${branch}`;
  const dir = await mkdtemp(join(tmpdir(), 'ci-screenshots-'));
  const index = join(dir, 'screenshots.index');

  const git = async (args: readonly string[], extraEnv: NodeJS.ProcessEnv = {}): Promise<string> => {
    const { stdout } = await execFileAsync('git', args, { cwd: dir, env: { ...env, ...extraEnv } });
    return stdout.trim();
  };

  const fetchTip = async (): Promise<string | undefined> => {
    const heads = await git(['ls-remote', 'origin', ref]);
    if (!heads.split('\n').some((line) => line.endsWith(`\t${ref}`))) {
      return undefined;
    }
    await git(['fetch', '--quiet', '--no-tags', '--depth=1', '--filter=blob:none', 'origin', `+${ref}:refs/remotes/origin/${branch}`]);
    return git(['rev-parse', '--verify', `refs/remotes/origin/${branch}^{commit}`]);
  };

  const buildTree = async (parent: string | undefined, blobs: readonly { path: string; oid: string }[]): Promise<string> => {
    const indexEnv = { GIT_INDEX_FILE: index };
    await rm(index, { force: true });
    await git(parent ? ['read-tree', parent] : ['read-tree', '--empty'], indexEnv);
    for (const blob of blobs) {
      await git(['update-index', '--add', '--cacheinfo', `100644,${blob.oid},${blob.path}`], indexEnv);
    }
    return git(['write-tree', '--missing-ok'], indexEnv);
  };

  try {
    await git(['init', '--quiet']);
    await git(['remote', 'add', 'origin', remote]);
    await git(['config', 'remote.origin.promisor', 'true']);
    await git(['config', 'remote.origin.partialclonefilter', 'blob:none']);
    const blobs = [];
    for (const file of files) {
      blobs.push({ path: file.path, oid: await git(['hash-object', '-w', '--', file.source]) });
    }

    for (let attempt = 1; ; attempt += 1) {
      try {
        const parent = await fetchTip();
        const tree = await buildTree(parent, blobs);
        if (parent && tree === (await git(['rev-parse', `${parent}^{tree}`]))) {
          log(`${branch}: ${parent} already has these files`);
          return parent;
        }
        const commit = await git(['commit-tree', tree, ...(parent ? ['-p', parent] : []), '-m', message]);
        await beforePush?.(attempt);
        await git(['push', '--quiet', 'origin', `${commit}:${ref}`]);
        log(`${branch}: pushed ${commit}${parent ? ` on top of ${parent}` : ' as the first commit'}`);
        return commit;
      } catch (error) {
        if (attempt >= attempts) {
          throw error;
        }
        const wait = retryDelayMs * attempt + Math.floor(Math.random() * retryDelayMs);
        log(`${branch}: attempt ${String(attempt)} failed, retrying on the latest tip in ${String(wait)} ms: ${firstLine(error)}`);
        await delay(wait);
      }
    }
  } finally {
    await rm(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

function firstLine(error: unknown): string {
  const text = error instanceof Error ? ((error as { stderr?: string }).stderr ?? error.message) : String(error);
  return text.trim().split('\n')[0] ?? '';
}
