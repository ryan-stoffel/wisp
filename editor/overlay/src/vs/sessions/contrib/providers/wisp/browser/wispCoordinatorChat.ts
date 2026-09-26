/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../../../base/common/cancellation.js';
import { Emitter } from '../../../../../base/common/event.js';
import { Disposable } from '../../../../../base/common/lifecycle.js';
import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import { IChatSession, IChatSessionContentProvider, IChatSessionsExtensionPoint, IChatSessionsService } from '../../../../../workbench/contrib/chat/common/chatSessionsService.js';
import { IWispHostStatusService } from '../../../wisp/browser/wispHostStatusService.js';
import { projectIdOf, WISP_PROJECT_SESSION_TYPE } from '../common/wispProjects.js';
import { NO_ATTACHMENTS } from './wispAgentChatSessions.js';
import { IWispProjectsService } from './wispProjectsService.js';

const WISP_COORDINATOR_AGENT_NAME = 'wisp-coordinator';

const COORDINATOR_PLACEHOLDER = localize('wispCoordinator.placeholder', "The coordinator can't take messages yet");

/**
 * Registers each project's coordinator thread with upstream's chat, in process (decision record
 * 0011): a chat session type for `wisp.project` resources and the provider of their content.
 *
 * Until the coordinator runs (M4) a thread has no turns, so it shows the spec's connected empty
 * state. Sending stays off through upstream's own rule: the type needs its own models and has
 * no Auto, and it has none until accounts come (M2), so the send action's precondition fails.
 */
export class WispCoordinatorChat extends Disposable implements IChatSessionContentProvider {

	constructor(
		@IChatSessionsService chatSessionsService: IChatSessionsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
	) {
		super();
		this._register(chatSessionsService.registerChatSessionContribution(this.contribution()));
		this._register(chatSessionsService.registerChatSessionContentProvider(WISP_PROJECT_SESSION_TYPE, this));
	}

	private contribution(): IChatSessionsExtensionPoint {
		const hostStatusService = this.hostStatusService;
		return {
			type: WISP_PROJECT_SESSION_TYPE,
			name: WISP_COORDINATOR_AGENT_NAME,
			displayName: localize('wispCoordinator.displayName', "Coordinator"),
			description: localize('wispCoordinator.description', "Plans a project's work and runs agents on its host"),
			welcomeTitle: localize('wispCoordinator.welcomeTitle', "Start with a goal"),
			// Read each time the empty state renders, so it names the host the project is on.
			get welcomeMessage() {
				const host = hostStatusService.status.get().host;
				return localize('wispCoordinator.welcomeMessage', "Tell the coordinator what you want done. It plans the work, runs agents in their own worktrees on {0}, and brings the changes back for you to review.\n\nThe coordinator comes in a later version of Wisp. Until then, this thread can't take messages.", host);
			},
			inputPlaceholder: COORDINATOR_PLACEHOLDER,
			canDelegate: false,
			supportsDelegation: false,
			requiresCustomModels: true,
			supportsAutoModel: false,
			requiresCopilotSignIn: false,
			autoAttachReferences: false,
			capabilities: NO_ATTACHMENTS,
		};
	}

	async provideChatSessionContent(sessionResource: URI, _token: CancellationToken): Promise<IChatSession> {
		const id = projectIdOf(sessionResource);
		const project = id !== undefined ? this.projectsService.getProject(id) : undefined;
		const onWillDispose = new Emitter<void>();
		return {
			sessionResource,
			title: project?.name,
			history: [],
			onWillDispose: onWillDispose.event,
			dispose: () => {
				onWillDispose.fire();
				onWillDispose.dispose();
			},
		};
	}
}
