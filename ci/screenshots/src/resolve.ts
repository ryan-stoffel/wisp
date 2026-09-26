import { appendFile, readFile } from 'node:fs/promises';
import { decide, formatRequest, wants, type Pull, type PullEvent } from './request.ts';
import { scenarios } from './scenarios.ts';

// screenshots.yml's request job: decides from a pull request (the REST API's JSON, read in the step
// before) and the event whether to capture, and what. It runs with a read-only token.

interface ApiPull {
  state: string;
  body: string | null;
  labels: { name: string }[];
  head: { sha: string; repo: { full_name: string } | null };
  base: { ref: string };
}

const pullPath = process.argv[2];
if (pullPath === undefined) {
  throw new Error('usage: resolve.ts <pull.json>');
}
const repository = required('GITHUB_REPOSITORY');
const eventName = required('GITHUB_EVENT_NAME');
const api: ApiPull = JSON.parse(await readFile(pullPath, 'utf8')) as ApiPull;
const payload = JSON.parse(await readFile(required('GITHUB_EVENT_PATH'), 'utf8')) as {
  action?: string;
  label?: { name?: string };
  changes?: PullEvent['changes'];
};

const pull: Pull = {
  state: api.state,
  headRepo: api.head.repo?.full_name,
  labels: api.labels.map((label) => label.name),
  body: api.body,
};
const event: PullEvent = { name: eventName, action: payload.action, label: payload.label?.name, changes: payload.changes };
const decision = decide(
  event,
  pull,
  repository,
  scenarios.map((scenario) => scenario.name),
);

const outputs = { run: 'false', publish: 'false', problem: '', request: '', 'base-sha': '' };
switch (decision.kind) {
  case 'skip':
    console.log(`::notice title=Screenshots skipped::${decision.reason}`);
    break;
  case 'problem':
    outputs.problem = decision.problem;
    outputs.publish = String(decision.publish);
    break;
  case 'run': {
    outputs.run = 'true';
    outputs.publish = 'true';
    outputs.request = formatRequest(decision.request);
    if (wants(decision.request, 'before-after')) {
      outputs['base-sha'] = await mergeBase(api.base.ref, api.head.sha);
    }
    console.log(`Capturing ${outputs.request}${outputs['base-sha'] ? `; the base app is ${outputs['base-sha']}` : ''}`);
    break;
  }
}

const lines = Object.entries(outputs).map(([key, value]) => `${key}=${value.replace(/[\r\n]+/g, ' ')}\n`);
await appendFile(required('GITHUB_OUTPUT'), lines.join(''));
if (process.env.GITHUB_STEP_SUMMARY) {
  const summary =
    decision.kind === 'run'
      ? `- Request: \`${outputs.request}\`${outputs['base-sha'] ? `\n- Base app: ${outputs['base-sha']}` : ''}\n`
      : `- ${decision.kind === 'skip' ? decision.reason : decision.problem}\n`;
  await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
}

/** The commit the head branched from, so the before differs from the after by this pull request only. */
async function mergeBase(baseRef: string, headSha: string): Promise<string> {
  const apiUrl = process.env.GITHUB_API_URL ?? 'https://api.github.com';
  const response = await fetch(`${apiUrl}/repos/${repository}/compare/${encodeURIComponent(baseRef)}...${headSha}?per_page=1`, {
    headers: {
      accept: 'application/vnd.github+json',
      authorization: `Bearer ${required('GH_TOKEN')}`,
      'user-agent': 'wisp-screenshots',
      'x-github-api-version': '2022-11-28',
    },
  });
  if (!response.ok) {
    throw new Error(`comparing ${baseRef} with ${headSha} returned ${String(response.status)}: ${await response.text()}`);
  }
  const sha = ((await response.json()) as { merge_base_commit?: { sha?: string } }).merge_base_commit?.sha ?? '';
  if (!/^[0-9a-f]{40}$/.test(sha)) {
    throw new Error(`comparing ${baseRef} with ${headSha} gave no merge base`);
  }
  return sha;
}

function required(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is not set; resolve.ts runs in screenshots.yml's request job`);
  }
  return value;
}
