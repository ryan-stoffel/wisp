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
import { isLocalHost } from '../../../../../platform/wisp/common/wispdConfiguration.js';
import type { Project } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ChatModelSource, IChat, ISession, ISessionType, ISessionWorkspace, ISessionWorkspaceBrowseAction, SessionRemoteConnectionFailureReason, SessionRemoteConnectionStatus } from '../../../../services/sessions/common/session.js';
import { ISendRequestOptions, ISessionChangeEvent, ISessionModelPickerOptions, ISessionModelsSnapshot, ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';
import { IWispHostStatusService } from '../../../wisp/browser/wispHostStatusService.js';
import { IWispProjectHost, WispProjectSession } from './wispProjectSession.js';
import { IWispProjectsService } from './wispProjectsService.js';

export const WISP_SESSIONS_PROVIDER_ID = 'wisp';

/**
 * wisp's sessions provider (decision record 0011): one `wisp.project` session per project on the
 * connected host, kept current by `IWispProjectsService`, which follows the editor's wispd
 * connection. It holds no logic of its own beyond that mapping.
 *
 * Its capabilities are what wisp can do today. Projects are created from the sidebar, not from
 * upstream's new-session composer, so it offers no session types, workspaces, or quick chats, and
 * it has no models until accounts come (M2). Anything else that would create or change a session
 * or a chat is refused.
 */
export class WispSessionsProvider extends Disposable implements ISessionsProvider {

	readonly id = WISP_SESSIONS_PROVIDER_ID;
	readonly label = localize('wispSessionsProvider.label', "Wisp");
	readonly icon: ThemeIcon = Codicon.server;
	readonly order = 0;

	readonly sessionTypes: readonly ISessionType[] = [];
	readonly onDidChangeSessionTypes: Event<void> = Event.None;
	readonly onDidChangeModels: Event<void> = Event.None;

	private readonly _onDidChangeSessions = this._register(new Emitter<ISessionChangeEvent>());
	readonly onDidChangeSessions: Event<ISessionChangeEvent> = this._onDidChangeSessions.event;

	readonly browseActions: readonly ISessionWorkspaceBrowseAction[] = [];
	readonly supportsLocalWorkspaces = false;
	readonly supportsQuickChats = false;

	/** By project id, in the order `project/list` gave them. */
	private readonly sessions = new Map<string, WispProjectSession>();
	private readonly connectionStatus: IObservable<SessionRemoteConnectionStatus>;

	constructor(
		@IWispProjectsService projectsService: IWispProjectsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
	) {
		super();
		this.connectionStatus = derived(this, reader => sessionConnectionStatus(hostStatusService.state.read(reader)));
		this._register(autorun(reader => this.sync(projectsService.projects.read(reader))));
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
				const session = new WispProjectSession(project, this.id, this.host());
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

	getSessions(): ISession[] {
		return [...this.sessions.values()];
	}

	getSessionTypes(_workspaceUri: URI): ISessionType[] {
		return [];
	}

	resolveWorkspace(_workspaceUri: URI): ISessionWorkspace | undefined {
		return undefined;
	}

	getModelsSnapshot(_sessionId: string, _desiredModelId?: string): ISessionModelsSnapshot {
		return { models: [], desiredModelResolution: { kind: 'notRequested' }, modelTarget: undefined };
	}

	getModelPickerOptions(_sessionId: string): ISessionModelPickerOptions {
		return { useGroupedModelPicker: false, showFeatured: false, showUnavailableFeatured: false, showManageModelsAction: false, showAutoModel: false };
	}

	createNewSession(_workspaceUri: URI, _sessionTypeId: string): ISession {
		throw notSupported();
	}

	createQuickChat(_sessionTypeId: string): ISession {
		throw notSupported();
	}

	deleteNewSession(_sessionId: string): void { }

	setModel(_sessionId: string, _chatResource: URI, _modelId: string, _source: ChatModelSource): void { }

	async renameChat(_sessionId: string, _chatUri: URI, _title: string): Promise<void> {
		throw notSupported();
	}

	async renameSession(_sessionId: string, _title: string): Promise<void> {
		throw notSupported();
	}

	async archiveSession(_sessionId: string): Promise<void> {
		throw notSupported();
	}

	async unarchiveSession(_sessionId: string): Promise<void> {
		throw notSupported();
	}

	async setSessionReadState(_sessionId: string, _isRead: boolean): Promise<void> {
		// Projects are always read until the coordinator runs (M4).
	}

	async deleteSession(_sessionId: string): Promise<void> {
		throw notSupported();
	}

	async deleteSessions(_sessionIds: readonly string[]): Promise<void> {
		throw notSupported();
	}

	async deleteChat(_sessionId: string, _chatUri: URI): Promise<boolean> {
		return false;
	}

	async createNewChat(_sessionId: string, _prompt?: string): Promise<IChat> {
		throw notSupported();
	}

	async forkChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw notSupported();
	}

	async createSideChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw notSupported();
	}

	async sendRequest(_sessionId: string, _chatResource: URI, _options: ISendRequestOptions): Promise<ISession> {
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
