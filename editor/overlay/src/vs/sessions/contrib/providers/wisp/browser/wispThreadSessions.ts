/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationError } from '../../../../../base/common/errors.js';
import { Codicon } from '../../../../../base/common/codicons.js';
import { Emitter, Event } from '../../../../../base/common/event.js';
import { Disposable } from '../../../../../base/common/lifecycle.js';
import { autorun, derived, IObservable, ISettableObservable, observableValue } from '../../../../../base/common/observable.js';
import { isAbsolute } from '../../../../../base/common/path.js';
import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import { INotificationService } from '../../../../../platform/notification/common/notification.js';
import { IQuickInputService, IQuickPickItem, IQuickPickSeparator } from '../../../../../platform/quickinput/common/quickInput.js';
import { generateUuidV7 } from '../../../../../platform/wisp/common/uuidv7.js';
import { agentReviewItems } from '../../../../../platform/wisp/common/wispAgentFiles.js';
import { IWispdService } from '../../../../../platform/wisp/common/wispd.js';
import type { Repo, RunId, Thread } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ISession, ISessionFileChange, ISessionType, ISessionWorkspace, ISessionWorkspaceBrowseAction, SessionRemoteConnectionStatus, SessionTypeAuthRequirement } from '../../../../services/sessions/common/session.js';
import { ISessionChangeEvent } from '../../../../services/sessions/common/sessionsProvider.js';
import { pickWorkerAccount } from '../../../wisp/browser/wispStartSubagent.js';
import { WISP_AGENT_CHAT_TYPE } from '../common/wispAgentRuns.js';
import { repoPathOf, WISP_THREAD_SESSION_TYPE } from '../common/wispThreads.js';
import { IWispAgentLocation } from './wispAgentChat.js';
import { IWispAgentsService } from './wispAgentsService.js';
import { IWispProjectsService } from './wispProjectsService.js';
import { IWispThreadsService } from './wispThreadsService.js';
import { IWispThreadContext, WispThreadSession, workspaceOfRepo } from './wispThreadSession.js';

/** The `wisp.thread` session type, as the new-session composer offers it. */
export const WISP_THREAD_TYPE: ISessionType = {
	id: WISP_THREAD_SESSION_TYPE,
	label: localize('wispThread.type', "Wisp"),
	icon: Codicon.commentDiscussion,
	// A thread's chat is a `wisp.agent` chat (0015), whose contribution needs no models.
	chatSessionType: WISP_AGENT_CHAT_TYPE,
	authRequirement: SessionTypeAuthRequirement.None,
};

/** The host the provider's sessions are on, as the thread sessions need it. */
export interface IWispThreadsHost {
	readonly isLocal: IObservable<boolean>;
	readonly connectionStatus: IObservable<SessionRemoteConnectionStatus>;
	readonly location: IObservable<IWispAgentLocation>;
}

interface IDiffEntry {
	commit: string | undefined;
	readonly value: ISettableObservable<readonly ISessionFileChange[]>;
}

type RepoPick = IQuickPickItem & { readonly path?: string; readonly other?: boolean };

/**
 * The thread half of wisp's sessions provider (decision record 0017): one `wisp.thread` session
 * per thread from `IWispThreadsService`, the new-session composer's drafts, the repo picker, and
 * sending a draft's first message as `thread/start`. `WispSessionsProvider` delegates to it.
 */
export class WispThreadSessions extends Disposable {

	private readonly _onDidChangeSessions = this._register(new Emitter<ISessionChangeEvent>());
	readonly onDidChangeSessions: Event<ISessionChangeEvent> = this._onDidChangeSessions.event;

	/** Threads by run id, in the order `thread/list` gave them. */
	private readonly sessions = new Map<RunId, WispThreadSession>();
	/** Drafts by session id, until they are sent or deleted. */
	private readonly drafts = new Map<string, WispThreadSession>();
	/** Each run's changed files, by the commit they were listed for. */
	private readonly diffs = new Map<RunId, IDiffEntry>();
	/** Drafts whose `thread/start` is on its way, by run id, so the thread adopts the draft. */
	private readonly sending = new Map<RunId, WispThreadSession>();

	readonly browseAction: ISessionWorkspaceBrowseAction;

	constructor(
		private readonly providerId: string,
		private readonly host: IWispThreadsHost,
		@IWispThreadsService private readonly threadsService: IWispThreadsService,
		@IWispAgentsService private readonly agentsService: IWispAgentsService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@IWispdService private readonly wispdService: IWispdService,
		@IQuickInputService private readonly quickInputService: IQuickInputService,
		@INotificationService private readonly notificationService: INotificationService,
	) {
		super();
		this.browseAction = {
			label: localize('wispThread.browse', "Repository on the Host..."),
			description: localize('wispThread.browseDescription', "A repository wispd can run agents in"),
			icon: Codicon.repo,
			providerId,
			run: () => this.pickRepo(),
		};
		this._register(autorun(reader => this.sync(threadsService.threads.read(reader), threadsService.repos.read(reader))));
	}

	get available(): boolean {
		return this.threadsService.available.get();
	}

	getSessions(): ISession[] {
		return [...this.sessions.values()];
	}

	getSession(sessionId: string): WispThreadSession | undefined {
		return this.drafts.get(sessionId) ?? [...this.sessions.values()].find(session => session.sessionId === sessionId);
	}

	/** The session of a thread, once it is listed. */
	getThreadSession(runId: RunId): WispThreadSession | undefined {
		return this.sessions.get(runId);
	}

	private context(): IWispThreadContext {
		return {
			isLocal: this.host.isLocal.get(),
			connectionStatus: this.host.connectionStatus,
			location: this.host.location,
			run: runId => derived(reader => {
				const scope = this.threadsService.threads.read(reader).find(thread => thread.id === runId)?.repo;
				return scope ? this.agentsService.runs(scope).read(reader).find(run => run.id === runId) : undefined;
			}),
			step: runId => this.agentsService.step(runId),
			changes: (runId, run) => derived(reader => {
				const entry = this.diffEntry(runId);
				const commit = run.read(reader)?.diff?.commit;
				if (commit && entry.commit !== commit) {
					entry.commit = commit;
					this.fetchChanges(runId, commit, entry);
				}
				return entry.value.read(reader);
			}),
		};
	}

	private diffEntry(runId: RunId): IDiffEntry {
		let entry = this.diffs.get(runId);
		if (!entry) {
			entry = { commit: undefined, value: observableValue<readonly ISessionFileChange[]>(`wispThreadChanges-${runId}`, []) };
			this.diffs.set(runId, entry);
		}
		return entry;
	}

	/**
	 * Lists the files of a run's commit with `agent/diff` (#157), each with a `wisp-agent:` URI on
	 * both sides, which the review's file system serves. A host without `agentReview` has none.
	 */
	private async fetchChanges(runId: RunId, commit: string, entry: IDiffEntry): Promise<void> {
		try {
			const diff = await this.wispdService.request('agent/diff', { runId });
			if (entry.commit !== commit) {
				return;
			}
			entry.value.set(agentReviewItems(runId, diff).map(item => ({
				uri: (item.modified ?? item.original)!,
				originalUri: item.original,
				modifiedUri: item.modified,
				insertions: item.file.insertions,
				deletions: item.file.deletions,
			})), undefined);
		} catch {
			// No review on this host, or the run's worktree is gone after an accept: no list.
			if (entry.commit === commit) {
				entry.value.set([], undefined);
			}
		}
	}

	private sync(threads: readonly Thread[], repos: readonly Repo[]): void {
		const added: ISession[] = [];
		const changed: ISession[] = [];
		const removed: ISession[] = [];
		const ids = new Set(threads.map(thread => thread.id));
		for (const [id, session] of this.sessions) {
			if (!ids.has(id)) {
				this.sessions.delete(id);
				removed.push(session);
			}
		}
		for (const thread of threads) {
			const existing = this.sessions.get(thread.id);
			if (existing) {
				if (existing.update(thread)) {
					changed.push(existing);
				}
				continue;
			}
			const draft = this.sending.get(thread.id);
			if (draft) {
				this.sending.delete(thread.id);
				draft.update(thread);
				this.sessions.set(thread.id, draft);
				added.push(draft);
				continue;
			}
			const repo = repos.find(candidate => candidate.id === thread.repo);
			if (!repo) {
				// Its entry's `repo.added` is on its way; the thread is listed once it arrives.
				continue;
			}
			const session = new WispThreadSession(thread.id, repo, thread, this.providerId, this.context());
			this.sessions.set(thread.id, session);
			added.push(session);
		}
		if (added.length || changed.length || removed.length) {
			this._onDidChangeSessions.fire({ added, changed, removed });
		}
	}

	/** A repository on the host as the composer's workspace, or `undefined` for a URI wisp doesn't own. */
	resolveWorkspace(uri: URI): ISessionWorkspace | undefined {
		if (!this.available) {
			return undefined;
		}
		const isLocal = this.host.isLocal.get();
		const path = repoPathOf(uri, isLocal);
		if (!path || !isAbsolute(path)) {
			return undefined;
		}
		const known = this.threadsService.repos.get().find(repo => !repo.scratch && repo.path === path);
		return workspaceOfRepo(known ?? { path, name: '' }, isLocal);
	}

	createDraft(workspaceUri: URI | undefined): ISession {
		if (!this.available) {
			throw new Error(localize('wispThread.unavailable', "The host's wispd can't run chats yet. Update wisp on the host."));
		}
		let repo: { readonly path: string; readonly name?: string } | undefined;
		if (workspaceUri) {
			const workspace = this.resolveWorkspace(workspaceUri);
			const path = workspace && repoPathOf(workspace.uri, this.host.isLocal.get());
			if (!workspace || !path) {
				throw new Error(localize('wispThread.notARepo', "Wisp can't run a chat in {0}.", workspaceUri.toString()));
			}
			repo = { path, name: workspace.label };
		}
		const session = new WispThreadSession(generateUuidV7(), repo, undefined, this.providerId, this.context());
		this.drafts.set(session.sessionId, session);
		return session;
	}

	deleteDraft(sessionId: string): void {
		this.drafts.delete(sessionId);
	}

	isDraft(sessionId: string): boolean {
		return this.drafts.has(sessionId);
	}

	/**
	 * Sends a draft's first message: registers its repository if needed, then `thread/start` with
	 * the draft's run id, so a retry never starts a second agent. The draft becomes the thread.
	 */
	async send(sessionId: string, prompt: string): Promise<ISession> {
		const draft = this.drafts.get(sessionId);
		if (!draft) {
			throw new Error(`no new chat has id ${sessionId}`);
		}
		const workspace = draft.workspace.get();
		const path = workspace && repoPathOf(workspace.uri, this.host.isLocal.get());
		const account = await pickWorkerAccount(this.wispdService, this.quickInputService, this.notificationService, localize('wispThread.accountTitle', "New Chat"));
		if (account === null) {
			throw new CancellationError();
		}
		const repo = path
			? this.threadsService.repos.get().find(candidate => !candidate.scratch && candidate.path === path) ?? await this.threadsService.addRepo(path)
			: undefined;
		this.sending.set(draft.runId, draft);
		try {
			const { thread } = await this.threadsService.start(draft.runId, repo?.id, prompt, account);
			// `start` added the thread, and `sync` adopted the draft; this covers a host change in
			// between, where the thread never reached the list.
			draft.update(thread);
		} finally {
			this.sending.delete(draft.runId);
		}
		this.drafts.delete(sessionId);
		return this.sessions.get(draft.runId) ?? draft;
	}

	async setArchived(sessionId: string, archived: boolean): Promise<void> {
		const session = this.getSession(sessionId);
		if (session?.thread) {
			await this.threadsService.setArchived(session.runId, archived);
		}
	}

	async delete(sessionId: string): Promise<void> {
		const session = this.getSession(sessionId);
		if (session?.thread) {
			await this.threadsService.delete(session.runId);
		}
	}

	/** Every repository wisp knows on the host, then a path typed in. */
	private async pickRepo(): Promise<ISessionWorkspace | undefined> {
		const isLocal = this.host.isLocal.get();
		const paths = new Map<string, string>();
		for (const repo of this.threadsService.repos.get()) {
			if (!repo.scratch) {
				paths.set(repo.path, repo.name);
			}
		}
		for (const project of this.projectsService.projects.get()) {
			if (!paths.has(project.repoPath)) {
				paths.set(project.repoPath, project.name);
			}
		}
		const items: Array<RepoPick | IQuickPickSeparator> = [...paths].map(([path, name]) => ({ label: name, description: path, path }));
		if (items.length) {
			items.push({ type: 'separator' });
		}
		items.push({ label: localize('wispThread.enterPath', "Enter a Path on the Host..."), other: true, alwaysShow: true });
		const picked = await this.quickInputService.pick(items, {
			title: localize('wispThread.pickTitle', "New Chat"),
			placeHolder: localize('wispThread.pickPlaceholder', "Pick the repository the agent works in"),
		});
		if (!picked) {
			return undefined;
		}
		let path = picked.path;
		if (picked.other) {
			path = await this.quickInputService.input({
				title: localize('wispThread.pathTitle', "New Chat"),
				prompt: localize('wispThread.pathPrompt', "The absolute path of a git repository on the host"),
				placeHolder: '/Users/me/src/app',
				validateInput: async value => isAbsolute(value.trim()) ? undefined : localize('wispThread.pathAbsolute', "Enter an absolute path, such as /Users/me/src/app."),
			});
			path = path?.trim();
		}
		if (!path) {
			return undefined;
		}
		const repo = await this.threadsService.addRepo(path);
		return workspaceOfRepo(repo, isLocal);
	}
}

