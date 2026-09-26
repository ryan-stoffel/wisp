/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ContextKeyExpr } from '../../../../platform/contextkey/common/contextkey.js';
import { ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService, IQuickPickItem } from '../../../../platform/quickinput/common/quickInput.js';
import { IWispdService } from '../../../../platform/wisp/common/wispd.js';
import type { AccountChoice, AgentRun } from '../../../../platform/wisp/common/wispProtocol.js';
import { IsSessionsWindowContext } from '../../../../workbench/common/contextkeys.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { ISessionsManagementService } from '../../../services/sessions/common/sessionsManagement.js';
import { IWispAgentsService } from '../../providers/wisp/browser/wispAgentsService.js';
import { WISP_RETRY_AGENT_COMMAND } from '../../providers/wisp/browser/wispAgentTranscript.js';
import { agentChatResource } from '../../providers/wisp/common/wispAgentRuns.js';
import { projectIdOf, projectResource } from '../../providers/wisp/common/wispProjects.js';
import { cliLabel } from './wispAccounts.js';

const WISP_START_SUBAGENT_COMMAND = 'wisp.startSubagent';

const category = localize2('wisp.category', "Wisp");
const hasAgents = ContextKeyExpr.and(IsSessionsWindowContext, ContextKeyExpr.deserialize(`'agents' in wisp.wispdCapabilities`));

/** Opens a run's chat as a tab next to its project's coordinator, once its session lists it. */
export async function openAgentTab(sessionsService: ISessionsService, sessionsManagementService: ISessionsManagementService, run: AgentRun): Promise<void> {
	const session = sessionsManagementService.getSessions().find(candidate => candidate.resource.toString() === projectResource(run.project).toString());
	if (session) {
		await sessionsService.openChat(session, agentChatResource(run.project, run.id));
	}
}

/**
 * The account a run started from this window goes on: none, so wispd uses the worker role's
 * default, or, when no default is set, one the user picks from the host's signed-in CLIs. Nothing
 * is saved: the default stays unset. Returns `null` when the user cancels or none can run it.
 */
export async function pickWorkerAccount(wispdService: IWispdService, quickInputService: IQuickInputService, notificationService: INotificationService, title = localize('wispStartSubagent.accountTitle', "Start Subagent")): Promise<AccountChoice | undefined | null> {
	const defaults = await wispdService.request('accounts/defaults/get', {});
	if (defaults.worker) {
		return undefined;
	}
	const { clis } = await wispdService.request('accounts/list', {});
	const picks: Array<IQuickPickItem & { readonly account: AccountChoice }> = clis
		.filter(cli => cli.installed && cli.signedIn !== false)
		.map(cli => ({ label: cliLabel(cli.cli), description: cli.plan, account: { kind: 'subscription', backend: cli.cli } }));
	if (picks.length === 0) {
		notificationService.info(localize('wispStartSubagent.noAccount', "No agent CLI is signed in on this host. Sign in to Claude Code from Customize, then Accounts."));
		return null;
	}
	const picked = await quickInputService.pick(picks, {
		title,
		placeHolder: localize('wispStartSubagent.accountPlaceholder', "Pick the account this agent runs on. No default is set for agents."),
	});
	return picked ? picked.account : null;
}

/** The account a run was on, to start it again on the same one. */
function accountOf(run: AgentRun): AccountChoice {
	return run.accountId === run.backend ? { kind: 'subscription', backend: run.backend } : { kind: 'key', id: run.accountId };
}

// Until the coordinator exists (M4), a developer command starts a subagent on the active project,
// so the Agents pill, the panel, and a subagent's tab can be tried end to end.
registerAction2(class StartSubagentAction extends Action2 {
	constructor() {
		super({
			id: WISP_START_SUBAGENT_COMMAND,
			title: localize2('wispStartSubagent.title', "Start Subagent"),
			category,
			f1: true,
			precondition: hasAgents,
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		const sessionsService = accessor.get(ISessionsService);
		const quickInputService = accessor.get(IQuickInputService);
		const notificationService = accessor.get(INotificationService);
		const agentsService = accessor.get(IWispAgentsService);
		const sessionsManagementService = accessor.get(ISessionsManagementService);
		const wispdService = accessor.get(IWispdService);
		const active = sessionsService.activeSession.get();
		const projectId = active ? projectIdOf(active.resource) : undefined;
		if (!projectId) {
			notificationService.info(localize('wispStartSubagent.noProject', "Open a project first. A subagent works in its project's repository."));
			return;
		}
		const prompt = await quickInputService.input({
			title: localize('wispStartSubagent.inputTitle', "Start Subagent"),
			prompt: localize('wispStartSubagent.inputPrompt', "The agent works on this in its own worktree of {0}.", active!.title.get()),
			placeHolder: localize('wispStartSubagent.placeholder', "Describe the task"),
			validateInput: async value => value.trim() ? undefined : localize('wispStartSubagent.empty', "Describe the task for the agent."),
		});
		if (!prompt?.trim()) {
			return;
		}
		let run: AgentRun;
		try {
			const account = await pickWorkerAccount(wispdService, quickInputService, notificationService);
			if (account === null) {
				return;
			}
			run = await agentsService.start(projectId, prompt.trim(), account);
		} catch (error) {
			notificationService.error(localize('wispStartSubagent.failed', "The agent didn't start: {0}", toErrorMessage(error)));
			return;
		}
		await openAgentTab(sessionsService, sessionsManagementService, run);
	}
});

registerAction2(class RetryAgentAction extends Action2 {
	constructor() {
		super({
			id: WISP_RETRY_AGENT_COMMAND,
			title: localize2('wispRetryAgent.title', "Retry Agent"),
			category,
			f1: false,
		});
	}

	async run(accessor: ServicesAccessor, runId?: unknown): Promise<void> {
		const agentsService = accessor.get(IWispAgentsService);
		const notificationService = accessor.get(INotificationService);
		const sessionsService = accessor.get(ISessionsService);
		const sessionsManagementService = accessor.get(ISessionsManagementService);
		const previous = typeof runId === 'string' ? agentsService.getRun(runId) : undefined;
		if (!previous) {
			return;
		}
		let run: AgentRun;
		try {
			run = await agentsService.start(previous.project, previous.prompt, accountOf(previous));
		} catch (error) {
			notificationService.error(localize('wispRetryAgent.failed', "The agent didn't start again: {0}", toErrorMessage(error)));
			return;
		}
		await openAgentTab(sessionsService, sessionsManagementService, run);
	}
});
