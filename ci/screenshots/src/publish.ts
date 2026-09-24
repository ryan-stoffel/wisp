import { appendFile, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { commitFiles, type BranchFile } from './branch.ts';
import { MARKER, renderComment, type FailedStep, type Images } from './comment.ts';
import { readManifest, type Manifest } from './manifest.ts';

const branch = 'ci-screenshots';
const bot = { name: 'github-actions[bot]', email: '41898282+github-actions[bot]@users.noreply.github.com' };

interface Comment {
  id: number;
  body?: string;
  html_url: string;
  user: { type: string } | null;
}

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    'dry-run': { type: 'boolean', default: false },
    logs: { type: 'string' },
  },
});
const dir = resolve(positionals[0] ?? join(import.meta.dirname, '..', 'out'));
const dryRun = values['dry-run'];

const serverUrl = process.env.GITHUB_SERVER_URL ?? 'https://github.com';
const apiUrl = process.env.GITHUB_API_URL ?? 'https://api.github.com';
const repository = dryRun ? (process.env.GITHUB_REPOSITORY ?? 'owner/repo') : required('GITHUB_REPOSITORY');
const prNumber = Number(dryRun ? (process.env.PR_NUMBER ?? '0') : required('PR_NUMBER'));
const headSha = dryRun ? (process.env.HEAD_SHA ?? '0'.repeat(40)) : required('HEAD_SHA');
const token = dryRun ? '' : required('GH_TOKEN');
const runUrl = `${serverUrl}/${repository}/actions/runs/${process.env.GITHUB_RUN_ID ?? '0'}`;
const prefix = `pr-${String(prNumber)}/${headSha.slice(0, 7)}`;

const manifest = await readManifest(dir);
const failedSteps = await readFailedSteps(values.logs);
const files = manifest ? publishable(manifest) : [];

let images: Images | undefined;
let pushError: string | undefined;
if (files.length > 0) {
  if (dryRun) {
    images = { base: `https://raw.githubusercontent.com/${repository}/<sha>/${prefix}`, tree: '<tree>' };
  } else {
    try {
      const sha = await commitFiles({
        remote: `${serverUrl}/${repository}.git`,
        branch,
        files,
        message: `chore: add screenshots for pr-${String(prNumber)} at ${headSha.slice(0, 7)}`,
        env: gitEnv(),
      });
      images = {
        base: `https://raw.githubusercontent.com/${repository}/${sha}/${prefix}`,
        tree: `${serverUrl}/${repository}/tree/${sha}/${prefix}`,
      };
    } catch (error) {
      pushError = error instanceof Error ? error.message : String(error);
      console.error(`pushing to ${branch} failed: ${pushError}`);
    }
  }
}

const body = renderComment({
  serverUrl,
  repository,
  prNumber,
  headSha,
  runUrl,
  manifest,
  images,
  failedSteps,
  ...(pushError === undefined ? {} : { pushError }),
});

if (dryRun) {
  process.stdout.write(body);
} else {
  await upsertComment(body);
}
if (pushError !== undefined) {
  process.exitCode = 1;
}

function publishable(manifest: Manifest): BranchFile[] {
  return manifest.results.flatMap((result) =>
    'file' in result && result.file ? [{ path: `${prefix}/${result.file}`, source: join(dir, result.file) }] : [],
  );
}

async function readFailedSteps(logs: string | undefined): Promise<FailedStep[]> {
  const steps = JSON.parse(process.env.STEPS_JSON ?? '{}') as Record<string, { outcome?: string }>;
  const failed: FailedStep[] = [];
  for (const [id, step] of Object.entries(steps)) {
    if (step.outcome !== 'failure') {
      continue;
    }
    const log = logs ? await readFile(join(logs, `${id}.log`), 'utf8').catch(() => undefined) : undefined;
    failed.push(log === undefined ? { id } : { id, log });
  }
  return failed;
}

async function upsertComment(body: string): Promise<void> {
  const pull = await github<{ head: { sha: string } }>('GET', `/repos/${repository}/pulls/${String(prNumber)}`);
  if (pull.head.sha !== headSha) {
    console.log(`PR head is now ${pull.head.sha}, not ${headSha}; leaving the comment to that commit's run`);
    return;
  }

  let existing: Comment | undefined;
  for (let page = 1; !existing; page += 1) {
    const comments = await github<Comment[]>(
      'GET',
      `/repos/${repository}/issues/${String(prNumber)}/comments?per_page=100&page=${String(page)}`,
    );
    existing = comments.find((comment) => comment.user?.type === 'Bot' && comment.body?.includes(MARKER));
    if (comments.length < 100) {
      break;
    }
  }

  const comment = existing
    ? await github<Comment>('PATCH', `/repos/${repository}/issues/comments/${String(existing.id)}`, { body })
    : await github<Comment>('POST', `/repos/${repository}/issues/${String(prNumber)}/comments`, { body });
  console.log(`${existing ? 'updated' : 'created'} ${comment.html_url}`);
  if (process.env.GITHUB_STEP_SUMMARY) {
    await appendFile(process.env.GITHUB_STEP_SUMMARY, `Screenshots comment: ${comment.html_url}\n`);
  }
}

async function github<T>(method: string, path: string, payload?: unknown): Promise<T> {
  const response = await fetch(`${apiUrl}${path}`, {
    method,
    headers: {
      accept: 'application/vnd.github+json',
      authorization: `Bearer ${token}`,
      'user-agent': 'wisp-screenshots',
      'x-github-api-version': '2022-11-28',
      ...(payload === undefined ? {} : { 'content-type': 'application/json' }),
    },
    ...(payload === undefined ? {} : { body: JSON.stringify(payload) }),
  });
  if (!response.ok) {
    throw new Error(`${method} ${path} returned ${String(response.status)}: ${await response.text()}`);
  }
  return (await response.json()) as T;
}

function gitEnv(): NodeJS.ProcessEnv {
  const basic = Buffer.from(`x-access-token:${token}`).toString('base64');
  if (process.env.GITHUB_ACTIONS === 'true') {
    console.log(`::add-mask::${basic}`);
  }
  return {
    ...process.env,
    GIT_TERMINAL_PROMPT: '0',
    GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: `http.${serverUrl}/.extraheader`,
    GIT_CONFIG_VALUE_0: `AUTHORIZATION: basic ${basic}`,
    GIT_AUTHOR_NAME: bot.name,
    GIT_AUTHOR_EMAIL: bot.email,
    GIT_COMMITTER_NAME: bot.name,
    GIT_COMMITTER_EMAIL: bot.email,
  };
}

function required(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is not set; publish-screenshots runs in screenshots.yml, or locally with --dry-run`);
  }
  return value;
}
