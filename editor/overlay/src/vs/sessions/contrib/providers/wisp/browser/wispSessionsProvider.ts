/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../../base/common/codicons.js';
import { Event } from '../../../../../base/common/event.js';
import { Disposable } from '../../../../../base/common/lifecycle.js';
import { ThemeIcon } from '../../../../../base/common/themables.js';
import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import { ChatModelSource, IChat, ISession, ISessionType, ISessionWorkspace, ISessionWorkspaceBrowseAction } from '../../../../services/sessions/common/session.js';
import { ISendRequestOptions, ISessionChangeEvent, ISessionModelPickerOptions, ISessionModelsSnapshot, ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';

export const WISP_SESSIONS_PROVIDER_ID = 'wisp';

/**
 * wisp's sessions provider (decision record 0011). Projects and threads come from wispd on a host,
 * and the Agents window has no wispd connection yet, so it has no session types, no sessions, and
 * no workspaces. Anything that would create or change a session is refused.
 */
export class WispSessionsProvider extends Disposable implements ISessionsProvider {

	readonly id = WISP_SESSIONS_PROVIDER_ID;
	readonly label = localize('wispSessionsProvider.label', "Wisp");
	readonly icon: ThemeIcon = Codicon.server;
	readonly order = 0;

	readonly sessionTypes: readonly ISessionType[] = [];
	readonly onDidChangeSessionTypes: Event<void> = Event.None;
	readonly onDidChangeSessions: Event<ISessionChangeEvent> = Event.None;
	readonly onDidChangeModels: Event<void> = Event.None;

	readonly browseActions: readonly ISessionWorkspaceBrowseAction[] = [];
	readonly supportsLocalWorkspaces = false;
	readonly supportsQuickChats = false;

	getSessions(): ISession[] {
		return [];
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
		throw noHost();
	}

	createQuickChat(_sessionTypeId: string): ISession {
		throw noHost();
	}

	deleteNewSession(_sessionId: string): void { }

	setModel(_sessionId: string, _chatResource: URI, _modelId: string, _source: ChatModelSource): void { }

	async renameChat(_sessionId: string, _chatUri: URI, _title: string): Promise<void> {
		throw noHost();
	}

	async renameSession(_sessionId: string, _title: string): Promise<void> {
		throw noHost();
	}

	async archiveSession(_sessionId: string): Promise<void> {
		throw noHost();
	}

	async unarchiveSession(_sessionId: string): Promise<void> {
		throw noHost();
	}

	async setSessionReadState(_sessionId: string, _isRead: boolean): Promise<void> {
		throw noHost();
	}

	async deleteSession(_sessionId: string): Promise<void> {
		throw noHost();
	}

	async deleteSessions(_sessionIds: readonly string[]): Promise<void> {
		throw noHost();
	}

	async deleteChat(_sessionId: string, _chatUri: URI): Promise<boolean> {
		return false;
	}

	async createNewChat(_sessionId: string, _prompt?: string): Promise<IChat> {
		throw noHost();
	}

	async forkChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw noHost();
	}

	async createSideChat(_sessionId: string, _sourceChat: URI, _turnId: string): Promise<IChat> {
		throw noHost();
	}

	async sendRequest(_sessionId: string, _chatResource: URI, _options: ISendRequestOptions): Promise<ISession> {
		throw noHost();
	}
}

function noHost(): Error {
	return new Error(localize('wispSessionsProvider.noHost', "No host connected"));
}
