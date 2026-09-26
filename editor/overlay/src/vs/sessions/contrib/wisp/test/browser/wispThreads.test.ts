/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { mainWindow } from '../../../../../base/browser/window.js';
import { URI } from '../../../../../base/common/uri.js';
import type { WispdState } from '../../../../../platform/wisp/common/wispd.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import type { AgentListParams, AgentRun, Repo, RepoAddParams, Thread, ThreadStartParams } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ISession, SessionStatus } from '../../../../services/sessions/common/session.js';
import { WispThreadSession } from '../../../providers/wisp/browser/wispThreadSession.js';
import { agentRunOf } from '../../../providers/wisp/common/wispAgentRuns.js';
import { placeThreadSessions, repoPathOf, repoUri, threadChatResource, threadResource, threadRunOf, WISP_REPO_SCHEME, WISP_THREAD_SESSION_TYPE } from '../../../providers/wisp/common/wispThreads.js';
import { WispThreadSections } from '../../browser/wispThreadSections.js';
import { agentsWindowServices, IAgentsWindowServices, settle } from './wispAgentsTestServices.js';
import { connected, SSH_COMMAND } from './wispHostTestUtils.js';

const APP: Repo = { id: '0192f0c4-0000-7000-8000-00000000a001', name: 'wisp', path: '/Users/ryan/src/wisp', createdAt: '2026-09-26T10:00:00Z' };
const SCRATCH: Repo = { id: '0192f0c4-0000-7000-8000-00000000a002', name: 'No Repo', path: '/Users/ryan/Library/Application Support/wisp/scratch', scratch: true, createdAt: '2026-09-26T10:00:00Z' };
const IN_REPO = '0192f0c4-0000-7000-8000-00000000b001';
const QUICK = '0192f0c4-0000-7000-8000-00000000b002';
const ARCHIVED = '0192f0c4-0000-7000-8000-00000000b003';

function thread(id: string, repo: Repo, options: Partial<Thread> = {}): Thread {
	return { id, repo: repo.id, createdAt: '2026-09-26T10:00:00Z', ...options };
}

function run(id: string, repo: Repo, prompt: string, options: Partial<AgentRun> = {}): AgentRun {
	return {
		id,
		project: repo.id,
		prompt,
		policy: 'workspaceWrite',
		status: 'running',
		backend: 'claude',
		accountId: 'claude',
		branch: `wisp/${id.slice(-4)}`,
		createdAt: '2026-09-26T10:00:00Z',
		updatedAt: '2026-09-26T10:00:00Z',
		...options,
	};
}

function withCapabilities(state: WispdState, capabilities: Record<string, Record<string, never>>): WispdState {
	return state.kind === 'connected' ? { ...state, capabilities } : state;
}

interface IThreadServices extends IAgentsWindowServices {
	readonly repos: Repo[];
	readonly threadList: Thread[];
	readonly runs: AgentRun[];
}

suite('wisp: threads', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	/** Services whose wispd has the `threads` capability, two repo entries, and three threads. */
	async function services(host = 'local'): Promise<IThreadServices> {
		const context = agentsWindowServices(disposables, true, host);
		const repos = [APP, SCRATCH];
		const threadList = [
			thread(IN_REPO, APP),
			thread(QUICK, SCRATCH),
			thread(ARCHIVED, APP, { archived: true }),
		];
		const runs = [
			run(IN_REPO, APP, 'Fix the flaky attach test', { updatedAt: '2026-09-26T10:05:00Z' }),
			run(QUICK, SCRATCH, 'What is a git worktree?', { status: 'completed', diff: { commit: 'c0ffee', files: 2, insertions: 3, deletions: 1 } }),
			run(ARCHIVED, APP, 'Old work', { status: 'completed' }),
		];
		context.wispd.handler = async (method, params) => {
			switch (method) {
				case 'project/list':
					return { projects: [], seq: 1 };
				case 'thread/list':
					return { repos, threads: threadList, seq: 3 };
				case 'agent/list':
					return { runs: runs.filter(candidate => candidate.project === (params as AgentListParams).project), seq: 3 };
				case 'agent/events':
					return { events: [], more: false };
				case 'agent/diff':
					return {
						base: 'ba5e', head: 'c0ffee', truncated: false, stats: { files: 2, insertions: 3, deletions: 1 },
						files: [
							{ path: 'NOTES.md', status: 'added', insertions: 2, deletions: 0 },
							{ path: 'README.md', status: 'modified', insertions: 1, deletions: 1 },
						],
					};
				case 'accounts/defaults/get':
					return { worker: { kind: 'subscription', backend: 'claude' } };
				case 'repo/add': {
					const { id, path } = params as RepoAddParams;
					return { repo: { id, name: path.split('/').pop()!, path, createdAt: '2026-09-26T11:00:00Z' } };
				}
				case 'thread/start': {
					const { runId, repo, prompt } = params as ThreadStartParams;
					const entry = repo ? { ...APP, id: repo } : SCRATCH;
					return { thread: thread(runId, entry), run: run(runId, entry, prompt, { status: 'starting' }) };
				}
				case 'thread/archive':
					return { thread: thread(IN_REPO, APP, { archived: true }) };
				case 'thread/delete':
					return {};
				case 'agent/cancel':
					return { run: run(IN_REPO, APP, 'Fix the flaky attach test', { status: 'cancelled' }) };
			}
			throw new Error(`unexpected ${method}`);
		};
		context.wispd.setState(withCapabilities(connected(host === 'local' ? undefined : SSH_COMMAND), { agents: {}, threads: {} }));
		await settle();
		return { ...context, repos, threadList, runs };
	}

	function threadSessions(context: IAgentsWindowServices): WispThreadSession[] {
		return context.provider.getSessions().filter((session): session is WispThreadSession => session.sessionType === WISP_THREAD_SESSION_TYPE);
	}

	suite('catalog', () => {

		test('resources name the thread and its run, and repositories map to workspace URIs', () => {
			assert.strictEqual(threadRunOf(threadResource(IN_REPO)), IN_REPO);
			assert.strictEqual(threadRunOf(URI.file('/x')), undefined);
			assert.deepStrictEqual(agentRunOf(threadChatResource(IN_REPO)), { projectId: 'thread', runId: IN_REPO });
			assert.strictEqual(repoUri('/src/app', true).scheme, 'file');
			assert.strictEqual(repoUri('/src/app', false).scheme, WISP_REPO_SCHEME);
			assert.strictEqual(repoPathOf(repoUri('/src/app', false), false), '/src/app');
			assert.strictEqual(repoPathOf(URI.file('/src/app'), false), undefined, 'a local folder is not a remote host\'s');
		});

		test('the provider publishes one wisp.thread session per thread, with its repo as its workspace', async () => {
			const context = await services();
			const sessions = threadSessions(context);
			assert.deepStrictEqual(sessions.map(session => [session.runId, session.title.get(), session.isQuickChat.get(), session.workspace.get()?.uri.toString(), session.isArchived.get()]), [
				[IN_REPO, 'Fix the flaky attach test', false, URI.file(APP.path).toString(), false],
				[QUICK, 'What is a git worktree?', true, undefined, false],
				[ARCHIVED, 'Old work', false, URI.file(APP.path).toString(), true],
			]);
			const [inRepo, quick] = sessions;
			assert.strictEqual(inRepo.resource.toString(), threadResource(IN_REPO).toString());
			assert.strictEqual(inRepo.mainChat.get().resource.toString(), threadChatResource(IN_REPO).toString());
			assert.strictEqual(inRepo.mainChat.get().origin, undefined, 'a thread\'s chat has no coordinator');
			assert.strictEqual(inRepo.status.get(), SessionStatus.InProgress);
			assert.strictEqual(quick.status.get(), SessionStatus.NeedsInput, 'a finished run with a commit needs review');
			assert.strictEqual(inRepo.workspace.get()?.label, 'wisp');
			assert.ok(inRepo.capabilities.get().supportsDelete);
			assert.deepStrictEqual(context.provider.sessionTypes.map(type => type.id), [WISP_THREAD_SESSION_TYPE]);
			assert.ok(context.provider.supportsQuickChats);
			assert.strictEqual(context.agents.getRun(IN_REPO)?.project, APP.id, 'the runs come from each repo entry\'s agent/list');
		});

		test('the Changes tab lists the files of the run\'s latest commit, from agent/diff', async () => {
			const context = await services();
			const quick = threadSessions(context).find(session => session.runId === QUICK)!;
			quick.changes.get();
			await settle();
			const changes = quick.mainChat.get().changes.get();
			assert.deepStrictEqual(changes.map(change => [change.originalUri?.toString(), change.modifiedUri?.toString(), change.insertions, change.deletions]), [
				[undefined, `wisp-agent://${QUICK}/head/NOTES.md?c0ffee`, 2, 0],
				[`wisp-agent://${QUICK}/base/README.md?ba5e`, `wisp-agent://${QUICK}/head/README.md?c0ffee`, 1, 1],
			]);
			assert.strictEqual(quick.changes.get(), changes);
			const inRepo = threadSessions(context).find(session => session.runId === IN_REPO)!;
			assert.deepStrictEqual(inRepo.changes.get(), [], 'a run with no commit has no changes');
		});

		test('a thread on another host names its repository without a local folder', async () => {
			const context = await services('mac-mini');
			const [inRepo] = threadSessions(context);
			const workspace = inRepo.workspace.get()!;
			assert.strictEqual(workspace.uri.scheme, WISP_REPO_SCHEME);
			assert.strictEqual(workspace.uri.path, APP.path);
			assert.ok(workspace.isVirtualWorkspace);
			assert.strictEqual(context.provider.supportsLocalWorkspaces, false);
		});

		test('without the threads capability, the provider offers no thread type or quick chats', async () => {
			const context = agentsWindowServices(disposables, true);
			context.wispd.handler = async method => method === 'project/list' ? { projects: [], seq: 1 } : Promise.reject(new Error(method));
			context.wispd.setState(withCapabilities(connected(), { agents: {} }));
			await settle();
			assert.deepStrictEqual(context.provider.sessionTypes, []);
			assert.strictEqual(context.provider.supportsQuickChats, false);
			assert.strictEqual(context.provider.resolveWorkspace(URI.file(APP.path)), undefined);
			assert.throws(() => context.provider.createQuickChat(WISP_THREAD_SESSION_TYPE));
		});
	});

	suite('placement', () => {

		test('repo threads go under their repository and quick chats under No Repo, leaving out archived ones', async () => {
			const context = await services();
			const placement = placeThreadSessions(context.provider.getSessions());
			assert.deepStrictEqual(placement.repositories.map(group => [group.label, group.sessions.map(session => (session as WispThreadSession).runId)]), [['wisp', [IN_REPO]]]);
			assert.deepStrictEqual(placement.noRepo.map(session => (session as WispThreadSession).runId), [QUICK]);
		});

		test('the sidebar lists them in Repositories and No Repo', async () => {
			const context = await services();
			const container = mainWindow.document.createElement('div');
			mainWindow.document.body.appendChild(container);
			try {
				disposables.add(context.instantiationService.createInstance(WispThreadSections, container, 'test'));
				const sections = [...container.querySelectorAll<HTMLElement>('section')];
				const read = (section: HTMLElement) => ({
					title: section.querySelector('h2')?.textContent,
					hidden: section.hidden,
					repos: [...section.querySelectorAll('.wisp-threads-repo-name')].map(heading => heading.textContent),
					rows: [...section.querySelectorAll('button.wisp-threads-row')].map(row => row.getAttribute('aria-label')?.split(',').slice(0, 2).join(',')),
				});
				assert.deepStrictEqual(sections.map(read), [
					{ title: 'Repositories', hidden: false, repos: ['wisp'], rows: ['Fix the flaky attach test, chat in wisp'] },
					{ title: 'No Repo', hidden: false, repos: [], rows: ['What is a git worktree?, chat'] },
				]);
				const rows = [...container.querySelectorAll<HTMLButtonElement>('button.wisp-threads-row')];
				assert.deepStrictEqual(rows.map(row => row.tabIndex), [0, 0], 'one tab stop per section');
				rows[0].click();
				assert.deepStrictEqual(context.opened.map(uri => uri.toString()), [threadResource(IN_REPO).toString()]);
			} finally {
				container.remove();
			}
		});
	});

	suite('new chat', () => {

		test('a draft in a repository registers it and starts the thread with the draft\'s run id', async () => {
			const context = await services();
			const path = '/Users/ryan/src/other';
			assert.ok(context.provider.resolveWorkspace(URI.file(path)));
			assert.deepStrictEqual(context.provider.getSessionTypes(URI.file(path)).map(type => type.id), [WISP_THREAD_SESSION_TYPE]);
			const draft = context.provider.createNewSession(URI.file(path), WISP_THREAD_SESSION_TYPE) as WispThreadSession;
			assert.strictEqual(draft.status.get(), SessionStatus.Untitled);
			assert.strictEqual(draft.workspace.get()?.uri.path, path);
			assert.ok(!context.provider.getSessions().includes(draft), 'a draft is not listed until it is sent');
			const chat = await context.provider.createNewChat(draft.sessionId);
			const sent = await context.provider.sendRequest(draft.sessionId, chat.resource, { query: 'Add a changelog' });
			await settle();
			assert.strictEqual(sent, draft, 'the draft becomes the thread');
			assert.ok(context.provider.getSessions().includes(draft));
			const starts = context.wispd.requests.filter(([method]) => method === 'repo/add' || method === 'thread/start');
			assert.deepStrictEqual(starts.map(([method]) => method), ['repo/add', 'thread/start']);
			const params = starts[1][1] as ThreadStartParams;
			assert.strictEqual(params.runId, draft.runId);
			assert.strictEqual(params.prompt, 'Add a changelog');
			assert.ok(params.repo);
			assert.strictEqual(draft.title.get(), 'Add a changelog');
			assert.strictEqual(draft.status.get(), SessionStatus.InProgress);
		});

		test('a known repository is not registered again, and a quick chat starts with no repo', async () => {
			const context = await services();
			const draft = context.provider.createNewSession(URI.file(APP.path), WISP_THREAD_SESSION_TYPE);
			await context.provider.sendRequest(draft.sessionId, draft.mainChat.get().resource, { query: 'Tidy the README' });
			const quick = context.provider.createQuickChat(WISP_THREAD_SESSION_TYPE);
			assert.ok(quick.isQuickChat?.get());
			assert.strictEqual(quick.workspace.get(), undefined);
			await context.provider.sendRequest(quick.sessionId, quick.mainChat.get().resource, { query: 'Explain rebase' });
			const starts = context.wispd.requests.filter(([method]) => method === 'repo/add' || method === 'thread/start').map(([method, params]) => [method, (params as ThreadStartParams).repo]);
			assert.deepStrictEqual(starts, [['thread/start', APP.id], ['thread/start', undefined]]);
		});

		test('a deleted draft is forgotten', async () => {
			const context = await services();
			const draft = context.provider.createQuickChat(WISP_THREAD_SESSION_TYPE);
			context.provider.deleteNewSession(draft.sessionId);
			await assert.rejects(() => context.provider.sendRequest(draft.sessionId, draft.mainChat.get().resource, { query: 'x' }));
		});
	});

	suite('archive and delete', () => {

		test('go through the provider to wispd, which stops a running agent itself', async () => {
			const context = await services();
			const [inRepo] = threadSessions(context);
			await context.provider.archiveSession(inRepo.sessionId);
			assert.ok(inRepo.isArchived.get());
			await context.provider.deleteSession(inRepo.sessionId);
			await settle();
			const calls = context.wispd.requests.map(([method]) => method).filter(method => method.startsWith('thread/') && method !== 'thread/list' || method === 'agent/cancel');
			assert.deepStrictEqual(calls, ['thread/archive', 'thread/delete']);
			assert.ok(!threadSessions(context).some((session: ISession) => session === inRepo), 'the deleted thread leaves the list');
			await assert.rejects(() => context.provider.archiveSession('wisp:wisp.project:/nope'));
		});
	});
});
