/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../../base/common/codicons.js';
import { IMarkdownString, MarkdownString } from '../../../../../base/common/htmlContent.js';
import { constObservable, derived, IObservable, ISettableObservable, observableValue, transaction } from '../../../../../base/common/observable.js';
import { basename } from '../../../../../base/common/path.js';
import { ThemeIcon } from '../../../../../base/common/themables.js';
import { URI } from '../../../../../base/common/uri.js';
import type { AgentRun, RunId, Thread } from '../../../../../platform/wisp/common/wispProtocol.js';
import { getUntitledSessionTitle, IChat, ISession, ISessionCapabilities, ISessionFileChange, ISessionWorkspace, SessionRemoteConnectionStatus, SessionStatus, toSessionId } from '../../../../services/sessions/common/session.js';
import { agentLocation, agentState, agentTitle, IWispAgentState, isRunActive } from '../common/wispAgentRuns.js';
import { repoUri, threadChatResource, threadResource, WISP_REPO_SCHEME, WISP_THREAD_SESSION_TYPE } from '../common/wispThreads.js';
import { IWispAgentLocation, WispChatBase, WispSessionBase } from './wispAgentChat.js';

/**
 * What wisp supports on a thread: one chat, archive, and delete. Renaming comes when wispd stores
 * a title (the prompt's first line is the title today).
 */
const WISP_THREAD_CAPABILITIES: ISessionCapabilities = {
	supportsMultipleChats: false,
	supportsFork: false,
	supportsSideChat: false,
	supportsRename: false,
	supportsDelete: true,
	supportsRemoveArtifacts: false,
};

/** Where a thread runs, and what it needs from `IWispAgentsService`. */
export interface IWispThreadContext {
	/** True when the host is this Mac, so a repository is a local folder. */
	readonly isLocal: boolean;
	readonly connectionStatus: IObservable<SessionRemoteConnectionStatus>;
	readonly location: IObservable<IWispAgentLocation>;
	/** The thread's run as `IWispAgentsService` has it, once the thread started. */
	run(runId: RunId): IObservable<AgentRun | undefined>;
	step(runId: RunId): IObservable<string | undefined>;
	/** The files the run's latest commit changed, from `agent/diff`, for the Changes tab. */
	changes(runId: RunId, run: IObservable<AgentRun | undefined>): IObservable<readonly ISessionFileChange[]>;
}

/**
 * A thread's one chat: its run's `wisp.agent` chat with no tool origin, so #105's transcript,
 * composer, and Stop serve it (0015). Before the thread starts it is an untitled draft. Its
 * `changes` are the files its latest commit changed, which the Changes tab lists.
 */
class WispThreadChat extends WispChatBase implements IChat {
	readonly resource: URI;
	readonly createdAt: Date;
	readonly title: IObservable<string>;
	readonly updatedAt: IObservable<Date>;
	readonly status: IObservable<SessionStatus>;
	readonly state: IObservable<IWispAgentState | undefined>;
	readonly isArchived: IObservable<boolean>;
	readonly description: IObservable<IMarkdownString | undefined>;
	readonly lastTurnEnd: IObservable<Date | undefined>;

	constructor(runId: RunId, createdAt: Date, run: IObservable<AgentRun | undefined>, step: IObservable<string | undefined>, where: IObservable<IWispAgentLocation>, untitled: IObservable<string>, archived: IObservable<boolean>, readonly changes: IObservable<readonly ISessionFileChange[]>) {
		super();
		this.resource = threadChatResource(runId);
		this.createdAt = createdAt;
		this.isArchived = archived;
		this.title = derived(this, reader => {
			const current = run.read(reader);
			return current ? agentTitle(current.prompt) : untitled.read(reader);
		});
		this.updatedAt = derived(this, reader => {
			const current = run.read(reader);
			return current ? new Date(current.updatedAt) : createdAt;
		});
		this.state = derived(this, reader => {
			const current = run.read(reader);
			return current ? agentState(current, step.read(reader)) : undefined;
		});
		this.status = derived(this, reader => this.state.read(reader)?.status ?? SessionStatus.Untitled);
		this.description = derived(this, reader => {
			const { isLocal, host } = where.read(reader);
			return new MarkdownString().appendText(agentLocation(isLocal, host));
		});
		this.lastTurnEnd = derived(this, reader => {
			const current = run.read(reader);
			return current && !isRunActive(current) ? new Date(current.updatedAt) : undefined;
		});
	}
}

/**
 * A normal thread as a session of the Agents window (decision records 0011 and 0017). A thread in
 * a repository has it as its workspace, so wisp's sidebar lists it under Repositories: a folder on
 * this Mac, or a `wisp-repo:` path on another host. A thread with no repo is a quick chat, with no
 * workspace, listed under No Repo.
 *
 * The same object is the new-session composer's draft and, once `thread/start` returns, the
 * thread: its id comes from the run id the draft generated, so the session keeps its identity
 * when it graduates into the list.
 */
export class WispThreadSession extends WispSessionBase implements ISession {
	readonly sessionId: string;
	readonly resource: URI;
	readonly providerId: string;
	readonly sessionType = WISP_THREAD_SESSION_TYPE;
	readonly icon: ThemeIcon = Codicon.commentDiscussion;
	readonly createdAt: Date;
	readonly workspace: IObservable<ISessionWorkspace | undefined>;
	readonly isQuickChat: IObservable<boolean>;
	readonly remoteConnectionStatus: IObservable<SessionRemoteConnectionStatus>;
	readonly title: IObservable<string>;
	readonly updatedAt: IObservable<Date>;
	readonly status: IObservable<SessionStatus>;
	readonly changes: IObservable<readonly ISessionFileChange[]>;
	readonly isArchived: IObservable<boolean>;
	readonly description: IObservable<IMarkdownString | undefined>;
	readonly lastTurnEnd: IObservable<Date | undefined>;
	readonly chats: IObservable<readonly IChat[]>;
	readonly mainChat: IObservable<IChat>;
	readonly capabilities = constObservable(WISP_THREAD_CAPABILITIES);

	private readonly _thread: ISettableObservable<Thread | undefined>;
	private readonly _started = observableValue<boolean>(this, false);
	private readonly chat: WispThreadChat;

	/**
	 * `repo` is the thread's repo entry, or for a draft the repository its workspace names; a
	 * scratch entry, or none, makes it a quick chat.
	 */
	constructor(
		readonly runId: RunId,
		repo: { readonly path: string; readonly name?: string; readonly scratch?: boolean } | undefined,
		thread: Thread | undefined,
		providerId: string,
		context: IWispThreadContext,
	) {
		super();
		this.resource = threadResource(runId);
		this.sessionId = toSessionId(providerId, this.resource);
		this.providerId = providerId;
		this.createdAt = thread ? new Date(thread.createdAt) : new Date();
		this._thread = observableValue<Thread | undefined>(this, thread);
		this._started.set(thread !== undefined, undefined);
		this.remoteConnectionStatus = context.connectionStatus;
		const quickChat = !repo || !!repo.scratch;
		this.isQuickChat = constObservable(quickChat);
		this.workspace = constObservable(quickChat ? undefined : repoWorkspace(repo, context.isLocal));
		this.isArchived = derived(this, reader => this._thread.read(reader)?.archived ?? false);
		const run = context.run(runId);
		const started = derived(this, reader => this._started.read(reader) ? run.read(reader) : undefined);
		this.chat = new WispThreadChat(runId, this.createdAt, started, context.step(runId), context.location, constObservable(getUntitledSessionTitle(quickChat)), this.isArchived, context.changes(runId, started));
		this.changes = this.chat.changes;
		this.mainChat = constObservable<IChat>(this.chat);
		this.chats = constObservable<readonly IChat[]>([this.chat]);
		this.title = this.chat.title;
		this.updatedAt = this.chat.updatedAt;
		this.status = this.chat.status;
		this.description = this.chat.description;
		this.lastTurnEnd = this.chat.lastTurnEnd;
	}

	get thread(): Thread | undefined {
		return this._thread.get();
	}

	/** The thread's state in the Agents panel's terms, once it started. */
	get state(): IObservable<IWispAgentState | undefined> {
		return this.chat.state;
	}

	/** Takes the thread as wispd reports it, and graduates a draft. Returns whether it changed. */
	update(thread: Thread): boolean {
		const changed = JSON.stringify(thread) !== JSON.stringify(this._thread.get());
		transaction(tx => {
			this._thread.set(thread, tx);
			this._started.set(true, tx);
		});
		return changed;
	}
}

/** A repo entry's workspace: a folder on this Mac, or a `wisp-repo:` path on another host. */
export function repoWorkspace({ path, name }: { readonly path: string; readonly name?: string }, isLocal: boolean): ISessionWorkspace {
	const uri = repoUri(path, isLocal);
	const label = name || basename(path) || path;
	return {
		uri,
		label,
		description: isLocal ? undefined : path,
		icon: Codicon.repo,
		folders: [{ root: uri, workingDirectory: uri, name: label, description: undefined }],
		requiresWorkspaceTrust: false,
		isVirtualWorkspace: uri.scheme === WISP_REPO_SCHEME,
	};
}
