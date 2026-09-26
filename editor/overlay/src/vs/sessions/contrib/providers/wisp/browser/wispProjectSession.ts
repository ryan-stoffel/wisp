/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../../base/common/codicons.js';
import { IMarkdownString } from '../../../../../base/common/htmlContent.js';
import { constObservable, derived, IObservable, ISettableObservable, observableValue } from '../../../../../base/common/observable.js';
import { basename } from '../../../../../base/common/path.js';
import { ThemeIcon } from '../../../../../base/common/themables.js';
import { URI } from '../../../../../base/common/uri.js';
import type { AgentRun, Project, RunId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ChatInteractivity, ChatModelSource, IChat, IChatCapabilities, IChatCheckpoints, ISession, ISessionCapabilities, ISessionChangeset, ISessionFileChange, ISessionWorkspace, SessionRemoteConnectionStatus, SessionStatus, toSessionId } from '../../../../services/sessions/common/session.js';
import { projectResource, WISP_PROJECT_SESSION_TYPE } from '../common/wispProjects.js';
import { IWispAgentLocation, WispAgentChat } from './wispAgentChat.js';

/**
 * What wisp supports on a project today: the coordinator, the subagents wispd runs for it (M3), and
 * nothing that changes a project. No one starts a new chat in a project from upstream's UI, so
 * `supportsMultipleChats` stays off. Each flag turns on with the milestone that gives wispd a
 * method for it.
 */
export const WISP_PROJECT_CAPABILITIES: ISessionCapabilities = {
	supportsMultipleChats: false,
	supportsFork: false,
	supportsSideChat: false,
	supportsRename: false,
	supportsDelete: false,
	supportsRemoveArtifacts: false,
};

const COORDINATOR_CHAT_CAPABILITIES: IChatCapabilities = { canRename: false, canDelete: false };

/** The host the provider's projects are on, as the sessions report it. */
export interface IWispProjectHost {
	/** True when the host is this Mac, so the repository is a local folder. */
	readonly isLocal: boolean;
	readonly connectionStatus: IObservable<SessionRemoteConnectionStatus>;
}

/** A project's agent runs, from `IWispAgentsService`, and where they run. */
export interface IWispProjectAgents {
	readonly runs: IObservable<readonly AgentRun[]>;
	step(runId: RunId): IObservable<string | undefined>;
	readonly location: IObservable<IWispAgentLocation>;
}

/**
 * A project's coordinator thread. It has no turns until the coordinator runs (M4), and it can't
 * be renamed or deleted apart from its project.
 */
class WispCoordinatorChat implements IChat {
	readonly resource: URI;
	readonly createdAt: Date;
	readonly title: IObservable<string>;
	readonly updatedAt: IObservable<Date>;
	readonly status = constObservable(SessionStatus.Completed);
	readonly changes = constObservable<readonly ISessionFileChange[]>([]);
	readonly checkpoints = constObservable<IChatCheckpoints | undefined>(undefined);
	readonly modelId = constObservable<string | undefined>(undefined);
	readonly modelSource = constObservable<ChatModelSource | undefined>(undefined);
	readonly mode = constObservable<{ readonly id: string; readonly kind: string } | undefined>(undefined);
	readonly isArchived = constObservable(false);
	readonly isRead = constObservable(true);
	readonly interactivity = constObservable(ChatInteractivity.Full);
	readonly description = constObservable<IMarkdownString | undefined>(undefined);
	readonly lastTurnEnd = constObservable<Date | undefined>(undefined);
	readonly capabilities = constObservable(COORDINATOR_CHAT_CAPABILITIES);

	constructor(resource: URI, createdAt: Date, project: IObservable<Project>) {
		this.resource = resource;
		this.createdAt = createdAt;
		this.title = derived(this, reader => project.read(reader).name);
		this.updatedAt = derived(this, reader => new Date(project.read(reader).updatedAt));
	}
}

/**
 * One wispd project as a session of the Agents window (decision record 0011). Its main chat is
 * the coordinator, and each agent run is a tool-origin chat after it (0015), which upstream lists
 * in the Agents pill and opens as a tab. A project on this Mac has its repository as its
 * workspace, so Files and **IDE** work; one on another host has none, since v1 can't open a
 * host's folders (#67).
 */
export class WispProjectSession implements ISession {
	readonly sessionId: string;
	readonly resource: URI;
	readonly providerId: string;
	readonly sessionType = WISP_PROJECT_SESSION_TYPE;
	readonly icon: ThemeIcon = Codicon.project;
	readonly createdAt: Date;
	readonly workspace: IObservable<ISessionWorkspace | undefined>;
	readonly isQuickChat = constObservable(false);
	readonly isAutomation = constObservable(false);
	readonly isExternal = constObservable(false);
	readonly remoteConnectionStatus: IObservable<SessionRemoteConnectionStatus>;
	readonly title: IObservable<string>;
	readonly updatedAt: IObservable<Date>;
	readonly status = constObservable(SessionStatus.Completed);
	readonly changes = constObservable<readonly ISessionFileChange[]>([]);
	readonly changesets = constObservable<readonly ISessionChangeset[] | undefined>(undefined);
	readonly modelId = constObservable<string | undefined>(undefined);
	readonly mode = constObservable<{ readonly id: string; readonly kind: string } | undefined>(undefined);
	readonly loading = constObservable(false);
	readonly isArchived = constObservable(false);
	readonly isRead = constObservable(true);
	readonly description = constObservable<IMarkdownString | undefined>(undefined);
	readonly lastTurnEnd = constObservable<Date | undefined>(undefined);
	readonly chats: IObservable<readonly IChat[]>;
	readonly mainChat: IObservable<IChat>;
	readonly capabilities = constObservable(WISP_PROJECT_CAPABILITIES);

	private readonly _project: ISettableObservable<Project>;
	/** One chat per run, kept so a chat's identity survives each update of the list. */
	private readonly agentChats = new Map<RunId, WispAgentChat>();

	constructor(project: Project, providerId: string, host: IWispProjectHost, agents?: IWispProjectAgents) {
		this._project = observableValue<Project>(this, project);
		this.resource = projectResource(project.id);
		this.sessionId = toSessionId(providerId, this.resource);
		this.providerId = providerId;
		this.createdAt = new Date(project.createdAt);
		this.remoteConnectionStatus = host.connectionStatus;
		this.title = derived(this, reader => this._project.read(reader).name);
		this.updatedAt = derived(this, reader => new Date(this._project.read(reader).updatedAt));
		// A project's repository never changes.
		this.workspace = constObservable(host.isLocal ? localWorkspace(project.repoPath) : undefined);
		const coordinator = new WispCoordinatorChat(this.resource, this.createdAt, this._project);
		this.mainChat = constObservable<IChat>(coordinator);
		// Subagents follow the coordinator in the order they started, which the Agents pill reverses.
		this.chats = agents
			? derived(this, reader => [coordinator, ...agents.runs.read(reader).map(run => this.agentChat(run, coordinator.resource, agents))])
			: constObservable<readonly IChat[]>([coordinator]);
	}

	private agentChat(run: AgentRun, parent: URI, agents: IWispProjectAgents): WispAgentChat {
		let chat = this.agentChats.get(run.id);
		if (!chat) {
			let last = run;
			const current = derived(this, reader => {
				last = agents.runs.read(reader).find(candidate => candidate.id === run.id) ?? last;
				return last;
			});
			chat = new WispAgentChat(run, parent, current, agents.step(run.id), agents.location);
			this.agentChats.set(run.id, chat);
		}
		return chat;
	}

	/** The chat of one of this project's runs, once its run is listed. */
	getAgentChat(runId: RunId): WispAgentChat | undefined {
		this.chats.get();
		return this.agentChats.get(runId);
	}

	get project(): Project {
		return this._project.get();
	}

	/** Takes a newer copy of the project. Returns whether anything changed. */
	update(project: Project): boolean {
		if (JSON.stringify(project) === JSON.stringify(this._project.get())) {
			return false;
		}
		this._project.set(project, undefined);
		return true;
	}
}

function localWorkspace(repoPath: string): ISessionWorkspace {
	const uri = URI.file(repoPath);
	const name = basename(repoPath) || repoPath;
	return {
		uri,
		label: name,
		icon: Codicon.repo,
		folders: [{ root: uri, workingDirectory: uri, name, description: undefined }],
		requiresWorkspaceTrust: false,
		isVirtualWorkspace: false,
	};
}
