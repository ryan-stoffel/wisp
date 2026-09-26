/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { MarkdownString } from '../../../../../base/common/htmlContent.js';
import { basename } from '../../../../../base/common/path.js';
import { localize } from '../../../../../nls.js';
import type { AgentOutcome, AgentOutputItem, AgentRun, DiffSummary, JsonValue, LoggedEvent, RunId, TurnId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { IChatProgress, IChatToolInvocationSerialized, ToolConfirmKind } from '../../../../../workbench/contrib/chat/common/chatService/chatService.js';
import { ToolDataSource } from '../../../../../workbench/contrib/chat/common/tools/languageModelToolsService.js';

/** The command #157 registers to open a run's changes in the diff review. */
export const WISP_REVIEW_AGENT_CHANGES_COMMAND = 'wisp.reviewAgentChanges';
/** Starts a new run with a finished run's task. */
export const WISP_RETRY_AGENT_COMMAND = 'wisp.retryAgent';

/** One request and its response in a subagent's chat. */
export interface IWispTranscriptTurn {
	/** The chat request's id: the turn id wispd logged, or one made from the run's id for its task. */
	readonly id: string;
	/** The `turnId` of a message sent with `agent/send`; `undefined` for the task. */
	readonly turnId: TurnId | undefined;
	/** The message. A message whose text this window doesn't know shows a placeholder. */
	readonly prompt: string;
	readonly parts: IChatProgress[];
	complete: boolean;
	startedAt: number | undefined;
	/** When wispd reported the turn finished, which can be before its response closes. */
	finishedAt: number | undefined;
	completedAt: number | undefined;
}

/** What one event changed, in order, for a chat that is already showing the transcript. */
export type WispTranscriptChange =
	| { readonly kind: 'turnStarted'; readonly turn: IWispTranscriptTurn }
	| { readonly kind: 'parts'; readonly turn: IWispTranscriptTurn; readonly parts: readonly IChatProgress[] }
	| { readonly kind: 'turnCompleted'; readonly turn: IWispTranscriptTurn }
	/** A message never reached the agent, so its turn never starts. */
	| { readonly kind: 'dropped'; readonly turnId: TurnId };

export interface IWispTranscriptOptions {
	/** The text of a message this window sent, by its turn id. */
	readonly sentText: (turnId: TurnId) => string | undefined;
	/** Whether #157's review command exists, so the card offers Review changes. */
	readonly canReview: () => boolean;
}

interface IPendingToolCall {
	readonly name: string;
	readonly input: JsonValue;
}

/**
 * A run's transcript, built one logged event at a time (decision record 0014): the task, then a
 * turn per message sent with `agent/send`. Parts are only ever appended to a turn, so a chat that
 * is showing it can stream the same parts it would have loaded as history.
 *
 * - `text` and `textDelta` are Markdown; `reasoning` is a thinking part.
 * - A tool call shows once its result arrives, or when its turn ends without one.
 * - `todoList`, `notice`, `warning`, and `followUpDropped` show as they come; `sessionStarted`
 *   and `usage` are not part of the transcript.
 * - A turn's response stays open until the CLI ends or the next message starts. Each time the
 *   CLI ends, a closing card says how, with Review changes and Retry. It waits for the run's new
 *   status, since wispd reports the commit (`agent.diffReady`) after `agent.finished`.
 */
export class WispAgentTranscript {

	readonly turns: IWispTranscriptTurn[] = [];
	private readonly seen = new Set<number>();
	private readonly pendingTools = new Map<string, IPendingToolCall>();
	/** Text already shown from `textDelta`s, by message id, so a later `text` isn't shown twice. */
	private readonly streamed = new Set<string>();
	private diff: DiffSummary | undefined;
	/** How the CLI ended, held until wispd has reported the commit that follows it. */
	private outcome: AgentOutcome | undefined;
	/** Whether the task's own `turnStarted` has arrived; its id is absent. */
	private taskStarted = false;

	constructor(
		private readonly run: Pick<AgentRun, 'id' | 'prompt' | 'createdAt' | 'diff'>,
		private readonly options: IWispTranscriptOptions,
	) {
		this.diff = run.diff;
		this.turns.push({
			id: taskTurnId(run.id),
			turnId: undefined,
			prompt: run.prompt,
			parts: [],
			complete: false,
			startedAt: Date.parse(run.createdAt) || undefined,
			finishedAt: undefined,
			completedAt: undefined,
		});
	}

	/** The turn that is showing now: the newest. */
	get current(): IWispTranscriptTurn {
		return this.turns[this.turns.length - 1];
	}

	/** Applies an event once. Events of other runs and kinds this editor doesn't know are skipped. */
	accept(logged: LoggedEvent): WispTranscriptChange[] {
		if (this.seen.has(logged.seq)) {
			return [];
		}
		this.seen.add(logged.seq);
		const event = logged.event;
		if (!('runId' in event) || event.runId !== this.run.id) {
			return [];
		}
		const time = Date.parse(logged.time) || undefined;
		switch (event.kind) {
			case 'agent.output': {
				const changes: WispTranscriptChange[] = [];
				for (const item of event.items) {
					changes.push(...this.item(item, time));
				}
				return changes;
			}
			case 'agent.diffReady':
				this.diff = event.diff;
				return [];
			case 'agent.finished':
				// wispd commits the worktree after the CLI ends, then reports the commit and the
				// run's new status, so the closing card waits for that status.
				this.outcome = event.outcome;
				return this.closeTools(time);
			case 'agent.updated':
				return event.state.status === 'starting' || event.state.status === 'running' ? [] : this.completeAll(time);
			default:
				return [];
		}
	}

	private item(item: AgentOutputItem, time: number | undefined): WispTranscriptChange[] {
		switch (item.kind) {
			case 'turnStarted':
				return this.turnStarted(item.turnId, time);
			case 'turnFinished': {
				// The turn's response stays open until the CLI ends or the next message starts, so the
				// closing card lands in it. The task's turn has no id.
				const turn = this.turns.find(candidate => candidate.turnId === item.turnId);
				if (turn && turn.finishedAt === undefined) {
					turn.finishedAt = time;
				}
				return [];
			}
			case 'textDelta':
				if (item.messageId) {
					this.streamed.add(item.messageId);
				}
				return this.append([markdown(item.text)]);
			case 'text':
				if (item.messageId && this.streamed.has(item.messageId)) {
					return [];
				}
				return this.append([markdown(item.text)]);
			case 'reasoning':
				return this.append([{ kind: 'thinking', value: item.text, ...(item.messageId ? { id: item.messageId } : {}) }]);
			case 'toolCall':
				this.pendingTools.set(item.callId, { name: item.name, input: item.input });
				return [];
			case 'toolResult': {
				const call = this.pendingTools.get(item.callId);
				this.pendingTools.delete(item.callId);
				return this.append([toolPart(item.callId, call?.name ?? localize('wispAgent.tool', "Tool"), call?.input ?? null, item.status, item.output)]);
			}
			case 'todoList':
				return this.append([{
					kind: 'toolInvocationSerialized',
					toolCallId: `todo-${this.current.id}-${this.current.parts.length}`,
					toolId: 'TodoWrite',
					source: ToolDataSource.Internal,
					invocationMessage: localize('wispAgent.todoUpdating', "Updating the checklist"),
					originMessage: undefined,
					pastTenseMessage: localize('wispAgent.todoUpdated', "Updated the checklist"),
					isConfirmed: { type: ToolConfirmKind.ConfirmationNotNeeded },
					isComplete: true,
					presentation: undefined,
					toolSpecificData: {
						kind: 'todoList',
						todoList: item.items.map((todo, index) => ({
							id: String(index),
							title: todo.text,
							status: todo.status === 'completed' ? 'completed' : todo.status === 'inProgress' ? 'in-progress' : 'not-started',
						})),
					},
				}]);
			case 'notice':
				return this.append([{ kind: 'info', content: new MarkdownString().appendText(item.detail) }]);
			case 'warning':
				return this.append([{ kind: 'warning', content: new MarkdownString().appendText(item.detail) }]);
			case 'followUpDropped':
				return [...this.append([droppedWarning()]), { kind: 'dropped', turnId: item.turnId }];
			default:
				// `sessionStarted`, `usage`, and kinds a newer wispd sends.
				return [];
		}
	}

	private turnStarted(turnId: TurnId | undefined, time: number | undefined): WispTranscriptChange[] {
		if (turnId === undefined && !this.taskStarted && this.turns.length === 1) {
			this.taskStarted = true;
			return [];
		}
		if (turnId !== undefined && this.turns.some(turn => turn.turnId === turnId)) {
			return [];
		}
		const changes = this.flushOutcome(time);
		changes.push(...this.closeTools(time));
		const previous = this.current;
		if (!previous.complete) {
			changes.push(...this.complete(previous, time));
		}
		const turn: IWispTranscriptTurn = {
			id: turnId ?? `${taskTurnId(this.run.id)}-${this.turns.length}`,
			turnId,
			prompt: (turnId && this.options.sentText(turnId)) || localize('wispAgent.unknownMessage', "A message sent to this agent"),
			parts: [],
			complete: false,
			startedAt: time,
			finishedAt: undefined,
			completedAt: undefined,
		};
		this.turns.push(turn);
		changes.push({ kind: 'turnStarted', turn });
		return changes;
	}

	/** Shows the closing card of the CLI that just ended, if one hasn't been shown. */
	private flushOutcome(time: number | undefined): WispTranscriptChange[] {
		const outcome = this.outcome;
		if (!outcome) {
			return [];
		}
		this.outcome = undefined;
		return [...this.closeTools(time), ...this.append(this.card(outcome))];
	}

	/**
	 * Shows the closing card, then closes every open turn. It runs when the run stops running,
	 * and the chat calls it when the run is no longer running but no `agent.updated` came, as for
	 * a run wispd found interrupted after a crash.
	 */
	completeAll(time: number | undefined): WispTranscriptChange[] {
		const changes: WispTranscriptChange[] = this.flushOutcome(time);
		for (const turn of this.turns) {
			if (!turn.complete) {
				changes.push(...this.complete(turn, time));
			}
		}
		return changes;
	}

	private card(outcome: AgentOutcome): IChatProgress[] {
		const retry = { id: WISP_RETRY_AGENT_COMMAND, title: localize('wispAgent.retry', "Retry"), arguments: [this.run.id] };
		const review = { id: WISP_REVIEW_AGENT_CHANGES_COMMAND, title: localize('wispAgent.review', "Review changes"), arguments: [this.run.id] };
		const changed = this.diff && this.diff.files > 0;
		const summary = !changed
			? localize('wispAgent.unchanged', "No files changed.")
			: this.diff!.files === 1
				? localize('wispAgent.changedOne', "1 file changed, +{0} -{1}.", this.diff!.insertions, this.diff!.deletions)
				: localize('wispAgent.changed', "{0} files changed, +{1} -{2}.", this.diff!.files, this.diff!.insertions, this.diff!.deletions);
		switch (outcome.status) {
			case 'completed': {
				const parts: IChatProgress[] = [{ kind: 'info', content: new MarkdownString().appendText(localize('wispAgent.finished', "Finished. {0}", summary)) }];
				if (changed && this.options.canReview()) {
					parts.push({ kind: 'command', command: review });
				}
				return parts;
			}
			case 'failed':
				return [
					{ kind: 'warning', content: new MarkdownString().appendText(localize('wispAgent.failedCard', "Failed: {0}", outcome.message)) },
					{ kind: 'command', command: retry, ...(changed && this.options.canReview() ? { additionalCommands: [review] } : {}) },
				];
			case 'cancelled':
				return [
					{ kind: 'info', content: new MarkdownString().appendText(localize('wispAgent.cancelledCard', "Stopped. {0}", summary)) },
					{ kind: 'command', command: retry, ...(changed && this.options.canReview() ? { additionalCommands: [review] } : {}) },
				];
			case 'interrupted':
				return [
					{ kind: 'info', content: new MarkdownString().appendText(localize('wispAgent.interruptedCard', "wispd stopped before this agent finished. Send a message to pick up where it left off.")) },
					{ kind: 'command', command: retry },
				];
			default:
				return [];
		}
	}

	/** Tool calls whose turn ended without a result show as they are. */
	private closeTools(_time: number | undefined): WispTranscriptChange[] {
		const parts: IChatProgress[] = [];
		for (const [callId, call] of this.pendingTools) {
			parts.push(toolPart(callId, call.name, call.input, undefined, undefined));
		}
		this.pendingTools.clear();
		return parts.length ? this.append(parts) : [];
	}

	private complete(turn: IWispTranscriptTurn, time: number | undefined): WispTranscriptChange[] {
		if (turn.complete) {
			return [];
		}
		const changes = turn === this.current ? this.closeTools(time) : [];
		turn.complete = true;
		turn.completedAt = turn.finishedAt ?? time;
		changes.push({ kind: 'turnCompleted', turn });
		return changes;
	}

	private append(parts: IChatProgress[]): WispTranscriptChange[] {
		const turn = this.current;
		turn.parts.push(...parts);
		return [{ kind: 'parts', turn, parts }];
	}
}

export function taskTurnId(runId: RunId): string {
	return `task-${runId}`;
}

export function droppedWarning(): IChatProgress {
	return { kind: 'warning', content: new MarkdownString().appendText(localize('wispAgent.followUpDropped', "A message didn't reach the agent before it stopped. Send it again.")) };
}

function markdown(text: string): IChatProgress {
	return { kind: 'markdownContent', content: new MarkdownString(text) };
}

function toolPart(callId: string, name: string, input: JsonValue, status: 'ok' | 'error' | 'denied' | undefined, output: string | undefined): IChatToolInvocationSerialized {
	const subject = toolSubject(input);
	const invocation = subject
		? localize('wispAgent.toolRunning', "{0} {1}", name, subject)
		: name;
	const past = status === 'denied'
		? localize('wispAgent.toolDenied', "{0} was denied", invocation)
		: status === 'error'
			? localize('wispAgent.toolFailed', "{0} failed", invocation)
			: invocation;
	return {
		kind: 'toolInvocationSerialized',
		toolCallId: callId,
		toolId: name,
		source: ToolDataSource.Internal,
		invocationMessage: invocation,
		originMessage: undefined,
		pastTenseMessage: past,
		isConfirmed: status === 'denied' ? { type: ToolConfirmKind.Denied } : { type: ToolConfirmKind.ConfirmationNotNeeded },
		isComplete: true,
		presentation: undefined,
		toolSpecificData: {
			kind: 'simpleToolInvocation',
			input: typeof input === 'string' ? input : JSON.stringify(input, undefined, 2),
			output: output ?? '',
		},
	};
}

/** What a tool call is about, for its one-line summary: a file's name, a command, or a pattern. */
export function toolSubject(input: JsonValue): string | undefined {
	if (!input || typeof input !== 'object' || Array.isArray(input)) {
		return undefined;
	}
	for (const key of ['file_path', 'path', 'notebook_path']) {
		const value = input[key];
		if (typeof value === 'string' && value) {
			return basename(value);
		}
	}
	for (const key of ['command', 'pattern', 'url', 'query', 'description']) {
		const value = input[key];
		if (typeof value === 'string' && value) {
			const line = value.split('\n')[0];
			return line.length > 60 ? `${line.slice(0, 59)}…` : line;
		}
	}
	return undefined;
}
