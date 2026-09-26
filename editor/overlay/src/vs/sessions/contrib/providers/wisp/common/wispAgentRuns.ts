/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import type { AgentRun, AgentRunState, AgentTodoItem, RunId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { SessionStatus } from '../../../../services/sessions/common/session.js';

/**
 * A subagent's chat type, and the scheme of its chat resource (decision record 0011): each run
 * that wispd reports under the `agents` capability is a tool-origin chat of its project's session.
 */
export const WISP_AGENT_CHAT_TYPE = 'wisp.agent';

export function agentChatResource(projectId: string, runId: RunId): URI {
	return URI.from({ scheme: WISP_AGENT_CHAT_TYPE, path: `/${projectId}/${runId}` });
}

/** The project and run in a subagent's chat resource, or `undefined` for any other resource. */
export function agentRunOf(resource: URI): { readonly projectId: string; readonly runId: RunId } | undefined {
	if (resource.scheme !== WISP_AGENT_CHAT_TYPE) {
		return undefined;
	}
	const match = /^\/([^/]+)\/([^/]+)$/.exec(resource.path);
	return match ? { projectId: match[1], runId: match[2] } : undefined;
}

/** A run with the latest `agent.updated` state applied. The prompt and ids never change. */
export function applyRunState(run: AgentRun, state: AgentRunState): AgentRun {
	const next: AgentRun = { ...run, status: state.status, accountId: state.accountId, updatedAt: state.updatedAt };
	if (state.sessionId !== undefined) {
		next.sessionId = state.sessionId;
	}
	if (state.error !== undefined) {
		next.error = state.error;
	} else {
		delete next.error;
	}
	if (state.diff !== undefined) {
		next.diff = state.diff;
	}
	return next;
}

/** A run's task title: the prompt's first non-empty line, cut to fit a tab and a panel row. */
export function agentTitle(prompt: string, max = 80): string {
	const line = prompt.split(/\r?\n/).map(part => part.trim()).find(part => part.length > 0) ?? '';
	if (!line) {
		return localize('wispAgent.untitled', "Agent");
	}
	return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line;
}

/**
 * The mark next to an agent's title in the Agents panel. Each has its own shape, so the state
 * never depends on color alone (docs/design/agents-window.md, States).
 */
export type WispAgentMark = 'hollow' | 'running' | 'filled' | 'done' | 'diamond' | 'square';

export interface IWispAgentState {
	readonly status: SessionStatus;
	readonly mark: WispAgentMark;
	/** The state as text: "Starting", the current step, "Needs review", "Failed", and so on. */
	readonly label: string;
	/** Whether the agent is working now, for the pill's running mark. */
	readonly running: boolean;
}

/** The checklist item an agent is on, for the state text of a running agent. */
export function currentStep(items: readonly AgentTodoItem[] | undefined): string | undefined {
	return items?.find(item => item.status === 'inProgress')?.text;
}

/**
 * How a run reads in the Agents panel, on its tab, and as `IChat.status`. A newer wispd may send
 * a status this editor doesn't know; it reads as the run's status word.
 */
export function agentState(run: Pick<AgentRun, 'status' | 'diff'>, step?: string): IWispAgentState {
	switch (run.status) {
		case 'starting':
			return { status: SessionStatus.InProgress, mark: 'hollow', label: localize('wispAgent.starting', "Starting"), running: true };
		case 'running':
			return { status: SessionStatus.InProgress, mark: 'running', label: step ?? localize('wispAgent.running', "Running"), running: true };
		case 'completed':
			return run.diff && run.diff.files > 0
				? { status: SessionStatus.NeedsInput, mark: 'filled', label: localize('wispAgent.needsReview', "Needs review"), running: false }
				: { status: SessionStatus.Completed, mark: 'done', label: localize('wispAgent.done', "Done"), running: false };
		case 'failed':
			return { status: SessionStatus.Error, mark: 'diamond', label: localize('wispAgent.failed', "Failed"), running: false };
		case 'cancelled':
			return { status: SessionStatus.Completed, mark: 'square', label: localize('wispAgent.stopped', "Stopped"), running: false };
		case 'interrupted':
			return { status: SessionStatus.Completed, mark: 'square', label: localize('wispAgent.interrupted', "Interrupted"), running: false };
		case 'accepted':
			return { status: SessionStatus.Completed, mark: 'done', label: localize('wispAgent.accepted', "Done"), running: false };
		default:
			return { status: SessionStatus.Completed, mark: 'square', label: String(run.status), running: false };
	}
}

/** Whether a run's CLI is working, so a new message is its next turn and Stop can cancel it. */
export function isRunActive(run: Pick<AgentRun, 'status'>): boolean {
	return run.status === 'starting' || run.status === 'running';
}

/** Where an agent runs, for its row and its tab: this Mac, or the host's name. */
export function agentLocation(isLocal: boolean, host: string): string {
	return isLocal ? localize('wispAgent.thisMac', "this Mac") : host;
}

/** A panel row's accessible name (docs/design/agents-window.md): "{title}, {state}, on {host}". */
export function agentAriaLabel(title: string, state: string, location: string): string {
	return localize('wispAgent.aria', "{0}, {1}, on {2}", title, state, location);
}
