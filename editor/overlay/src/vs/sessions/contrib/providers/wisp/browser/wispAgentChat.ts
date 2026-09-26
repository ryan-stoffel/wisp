/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { IMarkdownString, MarkdownString } from '../../../../../base/common/htmlContent.js';
import { constObservable, derived, IObservable } from '../../../../../base/common/observable.js';
import { URI } from '../../../../../base/common/uri.js';
import type { AgentRun } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ChatInteractivity, ChatModelSource, ChatOriginKind, IChat, IChatCapabilities, IChatCheckpoints, IChatOrigin, ISessionFileChange, SessionStatus } from '../../../../services/sessions/common/session.js';
import { agentChatResource, agentLocation, agentState, agentTitle, IWispAgentState, isRunActive } from '../common/wispAgentRuns.js';

const AGENT_CHAT_CAPABILITIES: IChatCapabilities = { canRename: false, canDelete: false };

/** Where the project's agents run, as its session sees the host. */
export interface IWispAgentLocation {
	readonly isLocal: boolean;
	/** The host's name, as the host chip shows it. */
	readonly host: string;
}

/**
 * A subagent, one wispd run, as a chat of its project's session (decision record 0011): tool
 * origin, with the coordinator as its parent, and fully interactive, so its tab has a composer.
 * wisp's sidebar and upstream's session list both skip tool-origin chats.
 *
 * - `status` and `state` follow the run and its current checklist step.
 * - `description` says where it runs: this Mac, or the host's name. The Agents pill shows it as
 *   the entry's badge.
 * - `changes` stays empty until the review lands (#157), which knows the worktree's files.
 */
export class WispAgentChat implements IChat {
	readonly resource: URI;
	readonly createdAt: Date;
	readonly title: IObservable<string>;
	readonly updatedAt: IObservable<Date>;
	readonly status: IObservable<SessionStatus>;
	readonly state: IObservable<IWispAgentState>;
	readonly changes = constObservable<readonly ISessionFileChange[]>([]);
	readonly checkpoints = constObservable<IChatCheckpoints | undefined>(undefined);
	readonly modelId = constObservable<string | undefined>(undefined);
	readonly modelSource = constObservable<ChatModelSource | undefined>(undefined);
	readonly mode = constObservable<{ readonly id: string; readonly kind: string } | undefined>(undefined);
	readonly isArchived = constObservable(false);
	readonly isRead = constObservable(true);
	readonly interactivity = constObservable(ChatInteractivity.Full);
	readonly description: IObservable<IMarkdownString | undefined>;
	readonly location: IObservable<string>;
	readonly lastTurnEnd: IObservable<Date | undefined>;
	readonly origin: IChatOrigin;
	readonly capabilities = constObservable(AGENT_CHAT_CAPABILITIES);

	constructor(
		initial: AgentRun,
		parentChat: URI,
		readonly run: IObservable<AgentRun>,
		step: IObservable<string | undefined>,
		where: IObservable<IWispAgentLocation>,
	) {
		this.resource = agentChatResource(initial.project, initial.id);
		this.createdAt = new Date(initial.createdAt);
		this.origin = { kind: ChatOriginKind.Tool, parentChat };
		// A run's prompt never changes.
		this.title = constObservable(agentTitle(initial.prompt));
		this.updatedAt = derived(this, reader => new Date(this.run.read(reader).updatedAt));
		this.state = derived(this, reader => agentState(this.run.read(reader), step.read(reader)));
		this.status = derived(this, reader => this.state.read(reader).status);
		this.location = derived(this, reader => {
			const { isLocal, host } = where.read(reader);
			return agentLocation(isLocal, host);
		});
		this.description = derived(this, reader => new MarkdownString().appendText(this.location.read(reader)));
		this.lastTurnEnd = derived(this, reader => {
			const run = this.run.read(reader);
			return isRunActive(run) ? undefined : new Date(run.updatedAt);
		});
	}
}
