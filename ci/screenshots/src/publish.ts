import { appendFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { missingArtifactError, readCapture, readLogTail, type Capture } from './artifact.ts';
import { commitFiles } from './branch.ts';
import { textProblem } from './manifest.ts';
import { renderSection, replaceSection, type FailedStep, type Images } from './section.ts';

const branch = 'ci-screenshots';
const bot = { login: 'github-actions[bot]', email: '41898282+github-actions[bot]@users.noreply.github.com' };
const steps = [
  { id: 'build', env: 'BUILD_OUTCOME' },
  { id: 'base-build', env: 'BASE_BUILD_OUTCOME' },
  { id: 'capture', env: 'CAPTURE_OUTCOME' },
] as const;
const maxProblemLength = 2_000;

interface Pull {
  body: string | null;
  html_url: string;
  head: { sha: string };
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
const baseSha = process.env.BASE_SHA ?? '';
const token = dryRun ? '' : required('GH_TOKEN');
if (!/^[0-9a-f]{40}$/.test(headSha) || !/^([0-9a-f]{40})?$/.test(baseSha) || !Number.isSafeInteger(prNumber) || prNumber < 0) {
  throw new Error('HEAD_SHA and BASE_SHA must be full commit SHAs and PR_NUMBER a pull request number');
}
// The request job ran the pull request's code, so the problem it reports is as untrusted as the artifact.
const rawProblem = process.env.REQUEST_PROBLEM ?? '';
const requestProblem =
  rawProblem === ''
    ? undefined
    : textProblem(rawProblem, maxProblemLength)
      ? 'the request job reported a problem that is not plain text; see its log.'
      : rawProblem;
const runUrl = `${serverUrl}/${repository}/actions/runs/${process.env.GITHUB_RUN_ID ?? '0'}`;
const prefix = `pr-${String(prNumber)}/${headSha.slice(0, 7)}`;

let capture: Capture | undefined;
let artifactError: string | undefined;
let images: Images | undefined;
let pushError: string | undefined;
const failedSteps = requestProblem === undefined ? await readFailedSteps(values.logs) : [];
if (requestProblem === undefined) {
  try {
    capture = await readCapture(dir);
  } catch (error) {
    artifactError = error instanceof Error ? error.message : String(error);
    console.error(`rejected the capture results in ${dir}: ${artifactError}`);
  }
  artifactError ??= missingArtifactError(capture, process.env.CAPTURE_OUTCOME);
}

if (capture && capture.files.length > 0) {
  const files = capture.files.map((file) => ({ path: `${prefix}/${file}`, source: join(dir, file) }));
  if (dryRun) {
    images = {
      raw: `https://raw.githubusercontent.com/${repository}/<sha>/${prefix}`,
      blob: `${serverUrl}/${repository}/blob/<sha>/${prefix}`,
      tree: '<tree>',
    };
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
        raw: `https://raw.githubusercontent.com/${repository}/${sha}/${prefix}`,
        blob: `${serverUrl}/${repository}/blob/${sha}/${prefix}`,
        tree: `${serverUrl}/${repository}/tree/${sha}/${prefix}`,
      };
    } catch (error) {
      pushError = error instanceof Error ? error.message : String(error);
      console.error(`pushing to ${branch} failed: ${pushError}`);
    }
  }
}

const section = renderSection({
  serverUrl,
  repository,
  prNumber,
  headSha,
  ...(baseSha === '' ? {} : { baseSha }),
  runUrl,
  manifest: capture?.manifest,
  images,
  failedSteps,
  ...(requestProblem === undefined ? {} : { requestProblem }),
  ...(pushError === undefined ? {} : { pushError }),
  ...(artifactError === undefined ? {} : { artifactError }),
});

if (dryRun) {
  process.stdout.write(`${section}\n`);
} else {
  await writeSection(section);
}
if (requestProblem !== undefined || pushError !== undefined || artifactError !== undefined) {
  process.exitCode = 1;
}

async function readFailedSteps(logs: string | undefined): Promise<FailedStep[]> {
  const failed: FailedStep[] = [];
  for (const step of steps) {
    if (process.env[step.env] !== 'failure') {
      continue;
    }
    const log = logs ? await readLogTail(logs, `${step.id}.log`) : undefined;
    failed.push(log === undefined ? { id: step.id } : { id: step.id, log });
  }
  return failed;
}

/**
 * Replaces the section in the pull request's body. The body is read again just before the write, so
 * an edit made while the scenes were captured is kept, and nothing is ever posted as a comment.
 */
async function writeSection(content: string): Promise<void> {
  const pull = await github<Pull>('GET', `/repos/${repository}/pulls/${String(prNumber)}`);
  if (pull.head.sha !== headSha) {
    console.log(`PR head is now ${pull.head.sha}, not ${headSha}; leaving the section to that commit's run`);
    return;
  }
  const body = replaceSection(pull.body, content);
  if (body === pull.body) {
    console.log('the section is already up to date');
    return;
  }
  await github<Pull>('PATCH', `/repos/${repository}/pulls/${String(prNumber)}`, { body });
  console.log(`wrote the Screenshots section of ${pull.html_url}`);
  if (process.env.GITHUB_STEP_SUMMARY) {
    await appendFile(process.env.GITHUB_STEP_SUMMARY, `Screenshots section: ${pull.html_url}\n`);
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
    GIT_AUTHOR_NAME: bot.login,
    GIT_AUTHOR_EMAIL: bot.email,
    GIT_COMMITTER_NAME: bot.login,
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
