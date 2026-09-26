/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../../base/common/codicons.js';
import { Emitter, Event } from '../../../../../base/common/event.js';
import { Disposable } from '../../../../../base/common/lifecycle.js';
import { autorun, derived, IObservable } from '../../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../../base/common/themables.js';
import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import { WispdState } from '../../../../../platform/wisp/common/wispd.js';
import { IInstantiationService } from '../../../../../platform/instantiation/common/instantiation.js';
import { isLocalHost } from '../../../../../platform/wisp/common/wispdConfiguration.js';
import type { Project } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ChatModelSource, IChat, ISession, ISessionType, ISessionWorkspace, ISessionWorkspaceBrowseAction, SessionRemoteConnectionFailureReason, SessionRemoteConnectionStatus } from '../../../../services/sessions/common/session.js';
import { ISendRequestOptions, ISessionChangeEvent, ISessionModelPickerOptions, ISessionModelsSnapshot, ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';
import { IWispHostStatusService } from '../../../wisp/browser/wispHostStatusService.js';
import { IWispAgentLocation } from './wispAgentChat.js';
import { IWispAgentsService } from './wispAgentsService.js';
import { IWispProjectAgents, IWispProjectHost, WispProjectSession } from './wispProjectSession.js';
import { IWispProjectsService } from './wispProjectsService.js';
import { IWispThreadsService } from './wispThreadsService.js';
import { WISP_THREAD_TYPE, WispThreadSessions } from './wispThreadSessions.js';

export const WISP_SESSIONS_PROVIDER_ID = 'wisp';

/**
 * wisp's sessions provider (decision record 0011): one `wisp.project` session per project on the
 * connected host, kept current by `IWispProjectsService`, which follows the editor's wispd
 * connection, with the project's agent runs from `IWispAgentsService` as its subagent chats (0015),
 * and one `wisp.thread` session per normal thread (0017), which `WispThreadSessions` keeps.
 *
 * Its capabilities are what wisp can do today. Projects are created from the sidebar. Normal
 * threads come from upstream's new-session composer: when the host has the `threads` capability,
 * the provider offers the `wisp.thread` type, repositories on the host as workspaces, and quick
 * chats for threads with no repo. It has no models until accounts come (M2). Anything else that
 * would create or change a session or a chat is refused.
 */
export class WispSessionsProvider extends Disposable implements ISessionsProvider {

	readonly id = WISP_SESSIONS_PROVIDER_ID;
	readonly label = localize('wispSessionsProvider.label', "Wisp");
	readonly icon: ThemeIcon = Codicon.server;
	readonly order = 0;

	readonly onDidChangeModels: Event<void> = Event.None;

	private readonly _onDidChangeSessions = this._register(new Emitter<ISessionChangeEvent>());
	readonly onDidChangeSessions: Event<ISessionChangeEvent> = this._onDidChangeSessions.event;

	private readonly _onDidChangeSessionTypes = this._register(new Emitter<void>());
	readonly onDidChangeSessionTypes: Event<void> = this._onDidChangeSessionTypes.event;
	private readonly _onDidChangeCapabilities = this._register(new Emitter<void>());
	readonly onDidChangeCapabilities: Event<void> = this._onDidChangeCapabilities.event;

	/** Normal threads and the composer's drafts (0017). */
	readonly threads: WispThreadSessions;

	/** By project id, in the order `project/list` gave them. */
	private readonly sessions = new Map<string, WispProjectSession>();
	private readonly connectionStatus: IObservable<SessionRemoteConnectionStatus>;
	private readonly location: IObservable<IWispAgentLocation>;

	constructor(
		@IWispProjectsService projectsService: IWispProjectsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IWispAgentsService private readonly agentsService: IWispAgentsService,
		@IWispThreadsService threadsService: IWispThreadsService,
		@IInstantiationService instantiationService: IInstantiationService,
	) {
		super();
		this.connectionStatus = derived(this, reader => sessionConnectionStatus(hostStatusService.state.read(reader)));
		this.location = derived(this, reader => ({
			isLocal: isLocalHost(hostStatusService.configuredHost.read(reader)),
			host: hostStatusService.status.read(reader).host,
		}));
		this._register(autorun(reader => this.sync(projectsService.projects.read(reader))));
		this.threads = this._register(instantiationService.createInstance(WispThreadSessions, this.id, {
			isLocal: derived(this, reader => isLocalHost(hostStatusService.configuredHost.read(reader))),
			connectionStatus: this.connectionStatus,
			location: this.location,
		}));
		this._register(this.threads.onDidChangeSessions(event => this._onDidChangeSessions.fire(event)));
		// The thread type, quick chats, and the repo picker follow the host's `threads` capability.
		let first = true;
		this._register(autorun(reader => {
			threadsService.available.read(reader);
			hostStatusService.configuredHost.read(reader);
			if (first) {
				first = false;
				return;
			}
			this._onDidChangeSessionTypes.fire();
			this._onDidChangeCapabilities.fire();
		}));
	}

	get sessionTypes(): readonly ISessionType[] {
		return this.threads.available ? [WISP_THREAD_TYPE] : [];
	}

	get browseActions(): readonly ISessionWorkspaceBrowseAction[] {
		return this.threads.available ? [this.threads.browseAction] : [];
	}

	/** A local host's folders can be picked with the native folder picker. */
	get supportsLocalWorkspaces(): boolean {
		return this.threads.available && isLocalHost(this.hostStatusService.configuredHost.get());
	}

	get supportsQuickChats(): boolean {
		return this.threads.available;
	}

	private sync(projects: readonly Project[]): void {
		const added: ISession[] = [];
		const changed: ISession[] = [];
		const removed: ISession[] = [];
		const ids = new Set(projects.map(project => project.id));
		for (const [id, session] of this.sessions) {
			if (!ids.has(id)) {
				this.sessions.delete(id);
				removed.push(session);
			}
		}
		for (const project of projects) {
			const existing = this.sessions.get(project.id);
			if (existing) {
				if (existing.update(project)) {
					changed.push(existing);
				}
			} else {
				const session = new WispProjectSession(project, this.id, this.host(), this.agents(project.id));
				this.sessions.set(project.id, session);
				added.push(session);
			}
		}
		if (added.length || changed.length || removed.length) {
			this._onDidChangeSessions.fire({ added, changed, removed });
		}
	}

	/** The host new sessions are on: the one configured when its projects arrived. */
	private host(): IWispProjectHost {
		return { isLocal: isLocalHost(this.hostStatusService.configuredHost.get()), connectionStatus: this.connectionStatus };
	}

	/** A project's runs, as its session lists them as subagent chats. */
	private agents(projectId: string): IWispProjectAgents {
		return {
			runs: this.agentsService.runs(projectId),
			step: runId => this.agentsService.step(runId),
			location: this.location,
		};
	}

	/** The session of a project on the connected host. */
	getProjectSession(projectId: string): WispProjectSession | undefined {
		return this.sessions.get(projectId);
	}

	getSessions(): ISession[] {
		return [...this.sessions.values(), ...this.threads.getSessions()];
	}

	getSessionTypes(workspaceUri: URI): ISessionType[] {
		return this.threads.resolveWorkspace(workspaceUri) ? [WISP_THREAD_TYPE] : [];
	}

	resolveWorkspace(workspaceUri: URI): ISessionWorkspace | undefined {
		return this.threads.resolveWorkspace(workspaceUri);
	}

	getModelsSnapshot(_sessionId: string, _desiredModelId?: string): ISessionModelsSnapshot {
		return { models: [], desiredModelResolution: { kind: 'notRequested' }, modelTarget: undefined };
	}

	getModelPickerOptions(sessionId: string): ISessionModelPickerOptions {
		// A thread's agent runs on the account wispd picks (0012), whose model is the CLI's.
		const auto = !!this.threads.getSession(sessionId);
		return { useGroupedModelPicker: false, showFeatured: false, showUnavailableFeatured: false, showManageModelsAction: false, showAutoModel: auto };
	}

	createNewSession(workspaceUri: URI, sessionTypeId: string): ISession {
		if (sessionTypeId !== WISP_THREAD_TYPE.id) {
			throw notSupported();
		}
		return this.threads.createDraft(workspaceUri);
	}

	createQuickChat(sessionTypeId: string): ISession {
		if (sessionTypeId !== WISP_THREAD_TYPE.id) {
			throw notSupported();
		}
		return this.threads.createDraft(undefined);
	}

	deleteNewSession(sessionId: string): void {
		this.threads.deleteDraft(sessionId);
	}

	setModel(_sessionId: string, _chatResource: URI, _modelId: string, _source: ChatModelSource): void { }

	async renameChat(_sessionId: string, _chatUri: URI, _title: string): Promise<void> {
		throw notSupported();
	}

	async renameSession(_sessionId: string, _title: string): Promise<void> {
		throw notSupported();
	}

	async archiveSession(sessionId: string): Promise<void> {
		if (!this.threads.getSession(sessionId)) {
			throw notSupported();
		}
		await this.threads.setArchived(sessionId, true);
	}

	async unarchiveSession(sessionId: string): Promise<void> {
		if (!this.threads.getSession(sessionId)) {
			throw notSupported();
		}
		await this.threads.setArchived(sessionId, false);
	}

	async setSessionReadState(_sessionId: string, _isRead: boolean): Promise<void> {
		// Projects are always read until the coordinator runs (M4).
	}

	async deleteSession(sessionId: string): Promise<void> {
		if (!this.threads.getSession(sessionId)) {
			throw notSupported();
		}
		await this.threads.delete(sessionId);
	}

	async deleteSessions(sessionIds: readonly string[]): Promise<void> {
		for (const sessionId of sessionIds) {
			await this.deleteSession(sessionId);
		}
	}

	async deleteChat(_sessionId: string, _chatUri: URI): Promise<boolean> {
		return false;
	}

	async createNewChat(sessionId: string, _prompt?: string): Promise<IChat> {
		// The composer's first message: a draft thread's one chat, which `sendRequest` starts.
		const draft = this.threads.isDraft(sessionId) ? this.threads.getSession(sessionId) : undefined;
		if (!draft) {
			throw notSupported();
		}
		return draft.mainChat.get();
	}

	async forkChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw notSupported();
	}

	async createSideChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw notSupported();
	}

	async sendRequest(sessionId: string, _chatResource: URI, options: ISendRequestOptions): Promise<ISession> {
		if (this.threads.isDraft(sessionId)) {
			return this.threads.send(sessionId, options.query);
		}
		throw new Error(localize('wispSessionsProvider.noCoordinator', "The coordinator can't take messages yet."));
	}
}

/** The connection as upstream's sessions see their host's. */
export function sessionConnectionStatus(state: WispdState): SessionRemoteConnectionStatus {
	switch (state.kind) {
		case 'connected':
			return { kind: 'connected' };
		case 'connecting':
			return state.attempt > 1 ? { kind: 'reconnecting' } : { kind: 'connecting' };
		case 'incompatible':
			return { kind: 'incompatible' };
		case 'disconnected':
			if (state.reason === 'notStarted') {
				return { kind: 'connecting' };
			}
			return state.retryAt !== undefined
				? { kind: 'reconnecting', nextAttemptAt: state.retryAt }
				: { kind: 'disconnected', reason: SessionRemoteConnectionFailureReason.Unknown };
	}
}

function notSupported(): Error {
	return new Error(localize('wispSessionsProvider.notSupported', "Wisp doesn't support this yet."));
}
