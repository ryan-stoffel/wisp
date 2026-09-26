/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../../../base/common/cancellation.js';
import { Codicon } from '../../../../../base/common/codicons.js';
import { toErrorMessage } from '../../../../../base/common/errorMessage.js';
import { Emitter } from '../../../../../base/common/event.js';
import { Disposable, IDisposable } from '../../../../../base/common/lifecycle.js';
import { autorun, constObservable, derived, IObservable, observableValue, transaction } from '../../../../../base/common/observable.js';
import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import { CommandsRegistry } from '../../../../../platform/commands/common/commands.js';
import { ExtensionIdentifier } from '../../../../../platform/extensions/common/extensions.js';
import { ILogService } from '../../../../../platform/log/common/log.js';
import { generateUuidV7 } from '../../../../../platform/wisp/common/uuidv7.js';
import type { AgentRun, LoggedEvent, RunId, TurnId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { IChatProgress } from '../../../../../workbench/contrib/chat/common/chatService/chatService.js';
import { IChatSession, IChatSessionContentProvider, IChatSessionHistoryItem, IChatSessionServerRequest, IChatSessionsExtensionPoint, IChatSessionsService } from '../../../../../workbench/contrib/chat/common/chatSessionsService.js';
import { ChatAgentLocation, ChatModeKind } from '../../../../../workbench/contrib/chat/common/constants.js';
import { IChatAgentData, IChatAgentRequest, IChatAgentResult, IChatAgentService } from '../../../../../workbench/contrib/chat/common/participants/chatAgents.js';
import { agentRunOf, agentTitle, isRunActive, WISP_AGENT_CHAT_TYPE } from '../common/wispAgentRuns.js';
import { IWispAgentsService } from './wispAgentsService.js';
import { droppedWarning, IWispTranscriptTurn, WISP_REVIEW_AGENT_CHANGES_COMMAND, WispAgentTranscript, WispTranscriptChange } from './wispAgentTranscript.js';

const WISP_AGENT_NAME = 'wisp-agent';

const AGENT_PLACEHOLDER = localize('wispAgent.placeholder', "Message this agent");

/** A message this window sent, whose turn streams into the request that sent it. */
interface IExpectedTurn {
	readonly progress: (parts: IChatProgress[]) => void;
	readonly done: () => void;
	started: boolean;
}

/**
 * One subagent's transcript as upstream's chat loads it: every turn but a running one as history,
 * and the running turn's parts through `progressObs`. Later events stream into it:
 *
 * - a turn started by a message this window sent goes to that request (`expect`);
 * - any other new turn, such as a message from another window or wispd's resume after a
 *   restart, starts a server request;
 * - Stop cancels the run.
 */
export class WispAgentChatSession extends Disposable implements IChatSession {

	readonly progressObs = observableValue<IChatProgress[]>(this, []);
	readonly isCompleteObs = observableValue<boolean>(this, true);
	readonly history: readonly IChatSessionHistoryItem[];
	readonly title: string;

	private readonly _onWillDispose = this._register(new Emitter<void>());
	readonly onWillDispose = this._onWillDispose.event;

	private readonly _onDidStartServerRequest = this._register(new Emitter<IChatSessionServerRequest>());
	readonly onDidStartServerRequest = this._onDidStartServerRequest.event;

	/** The turn `progressObs` streams, if any. */
	private streaming: IWispTranscriptTurn | undefined;
	private readonly expected = new Map<TurnId, IExpectedTurn>();

	constructor(
		readonly sessionResource: URI,
		private readonly transcript: WispAgentTranscript,
		run: IObservable<AgentRun | undefined>,
		private readonly cancel: () => Promise<unknown>,
	) {
		super();
		this.title = transcript.turns[0].prompt ? agentTitle(transcript.turns[0].prompt) : '';
		const history: IChatSessionHistoryItem[] = [];
		for (const turn of transcript.turns) {
			history.push(request(turn));
			if (turn.complete) {
				history.push(response(turn));
			} else {
				this.streaming = turn;
				transaction(tx => {
					this.progressObs.set([...turn.parts], tx);
					this.isCompleteObs.set(false, tx);
				});
			}
		}
		this.history = history;

		// A run that ended without an `agent.finished`, as one interrupted by a crash, still closes.
		this._register(autorun(reader => {
			const current = run.read(reader);
			if (current && !isRunActive(current)) {
				this.apply(this.transcript.completeAll(Date.parse(current.updatedAt) || undefined));
			}
		}));
	}

	readonly interruptActiveResponseCallback = async (): Promise<boolean> => {
		await this.cancel();
		return true;
	};

	/** Takes a logged event of this run. */
	accept(event: LoggedEvent): void {
		this.apply(this.transcript.accept(event));
	}

	/**
	 * Routes the turn of a message this window is about to send to the request that sends it.
	 * Returns a promise that settles when that turn's response closes.
	 */
	expect(turnId: TurnId, progress: (parts: IChatProgress[]) => void, token: CancellationToken): { readonly done: Promise<void>; readonly dispose: () => void } {
		let resolve!: () => void;
		const done = new Promise<void>(r => resolve = r);
		const entry: IExpectedTurn = { progress, done: resolve, started: false };
		this.expected.set(turnId, entry);
		const listener = token.onCancellationRequested(() => resolve());
		return {
			done,
			dispose: () => {
				listener.dispose();
				if (this.expected.get(turnId) === entry) {
					this.expected.delete(turnId);
				}
			},
		};
	}

	private apply(changes: readonly WispTranscriptChange[]): void {
		for (const change of changes) {
			if (change.kind === 'dropped') {
				const dropped = this.expected.get(change.turnId);
				if (dropped && !dropped.started) {
					this.expected.delete(change.turnId);
					dropped.progress([droppedWarning()]);
					dropped.done();
				}
				continue;
			}
			const expected = change.turn.turnId !== undefined ? this.expected.get(change.turn.turnId) : undefined;
			switch (change.kind) {
				case 'turnStarted':
					if (expected) {
						expected.started = true;
						expected.progress([...change.turn.parts]);
					} else {
						this.startServerRequest(change.turn);
					}
					break;
				case 'parts':
					if (expected?.started) {
						expected.progress([...change.parts]);
					} else if (change.turn === this.streaming) {
						this.progressObs.set([...this.progressObs.get(), ...change.parts], undefined);
					}
					break;
				case 'turnCompleted':
					if (expected?.started) {
						this.expected.delete(change.turn.turnId!);
						expected.done();
					} else if (change.turn === this.streaming) {
						this.streaming = undefined;
						this.isCompleteObs.set(true, undefined);
					}
					break;
			}
		}
	}

	private startServerRequest(turn: IWispTranscriptTurn): void {
		this.streaming = turn;
		transaction(tx => {
			this.progressObs.set([...turn.parts], tx);
			this.isCompleteObs.set(false, tx);
		});
		this._onDidStartServerRequest.fire({ id: turn.id, prompt: turn.prompt, timestamp: turn.startedAt });
	}

	override dispose(): void {
		if (!this._store.isDisposed) {
			this._onWillDispose.fire();
		}
		for (const expected of this.expected.values()) {
			expected.done();
		}
		this.expected.clear();
		super.dispose();
	}
}

function request(turn: IWispTranscriptTurn): IChatSessionHistoryItem {
	return { type: 'request', id: turn.id, prompt: turn.prompt, participant: WISP_AGENT_CHAT_TYPE, ...(turn.startedAt !== undefined ? { timestamp: turn.startedAt } : {}) };
}

function response(turn: IWispTranscriptTurn): IChatSessionHistoryItem {
	return {
		type: 'response',
		parts: [...turn.parts],
		participant: WISP_AGENT_CHAT_TYPE,
		...(turn.completedAt !== undefined ? { completedAt: turn.completedAt } : {}),
		...(turn.completedAt !== undefined && turn.startedAt !== undefined ? { elapsedMs: Math.max(0, turn.completedAt - turn.startedAt) } : {}),
	};
}

/**
 * Registers subagents with upstream's chat, in process (decision record 0011): the `wisp.agent`
 * chat session type, the provider of each run's transcript, and the chat agent that takes the
 * composer's messages. A message goes out as `agent/send` with a new turn id, so the connection's
 * resend after a reconnect can't deliver it twice, and Stop sends `agent/cancel`.
 */
export class WispAgentChatSessions extends Disposable implements IChatSessionContentProvider {

	private readonly sessions = new Map<RunId, Set<WispAgentChatSession>>();

	constructor(
		@IChatSessionsService chatSessionsService: IChatSessionsService,
		@IChatAgentService chatAgentService: IChatAgentService,
		@IWispAgentsService private readonly agentsService: IWispAgentsService,
		@ILogService private readonly logService: ILogService,
	) {
		super();
		this._register(chatSessionsService.registerChatSessionContribution(contribution()));
		this._register(chatSessionsService.registerChatSessionContentProvider(WISP_AGENT_CHAT_TYPE, this));
		this._register(chatAgentService.registerDynamicAgent(agentData(), {
			invoke: (request, progress, _history, token) => this.invoke(request, progress, token),
		}));
		this._register(agentsService.onDidAddEvent(({ runId, event }) => {
			for (const session of this.sessions.get(runId) ?? []) {
				session.accept(event);
			}
		}));
	}

	async provideChatSessionContent(sessionResource: URI, _token: CancellationToken): Promise<IChatSession> {
		const ids = agentRunOf(sessionResource);
		if (!ids) {
			throw new Error(`not a subagent: ${sessionResource.toString()}`);
		}
		const { runId } = ids;
		let events: readonly LoggedEvent[] = [];
		try {
			events = await this.agentsService.loadEvents(runId);
		} catch (error) {
			this.logService.error(`[wisp] couldn't load the transcript of run ${runId}`, error);
		}
		const run = this.agentsService.getRun(runId);
		const transcript = new WispAgentTranscript(
			run ?? { id: runId, prompt: '', createdAt: new Date().toISOString() },
			{
				sentText: turnId => this.agentsService.sentText(turnId),
				canReview: () => !!CommandsRegistry.getCommand(WISP_REVIEW_AGENT_CHANGES_COMMAND),
			},
		);
		for (const event of events) {
			transcript.accept(event);
		}
		const runs = run ? this.agentsService.runs(run.project) : undefined;
		const current = runs ? derived(reader => runs.read(reader).find(candidate => candidate.id === runId)) : constObservable<AgentRun | undefined>(undefined);
		const session = new WispAgentChatSession(sessionResource, transcript, current, () => this.agentsService.cancel(runId));
		let set = this.sessions.get(runId);
		if (!set) {
			set = new Set();
			this.sessions.set(runId, set);
		}
		set.add(session);
		const listener: IDisposable = session.onWillDispose(() => {
			listener.dispose();
			set.delete(session);
			if (set.size === 0 && this.sessions.get(runId) === set) {
				this.sessions.delete(runId);
			}
		});
		return session;
	}

	private async invoke(request: IChatAgentRequest, progress: (parts: IChatProgress[]) => void, token: CancellationToken): Promise<IChatAgentResult> {
		const ids = agentRunOf(request.sessionResource);
		if (!ids) {
			return { errorDetails: { message: localize('wispAgent.notAgent', "This chat isn't an agent.") } };
		}
		const session = [...this.sessions.get(ids.runId) ?? []].at(-1);
		const turnId = generateUuidV7();
		const expected = session?.expect(turnId, progress, token);
		const cancel = token.onCancellationRequested(() => {
			this.agentsService.cancel(ids.runId).catch(error => this.logService.error(`[wisp] agent/cancel failed for run ${ids.runId}`, error));
		});
		try {
			await this.agentsService.send(ids.runId, turnId, request.message);
			await expected?.done;
			return {};
		} catch (error) {
			return { errorDetails: { message: localize('wispAgent.sendFailed', "The message didn't reach the agent: {0}", toErrorMessage(error)) } };
		} finally {
			cancel.dispose();
			expected?.dispose();
		}
	}
}

/** A wisp chat takes text only. */
export const NO_ATTACHMENTS: IChatSessionsExtensionPoint['capabilities'] = {
	supportsFileAttachments: false,
	supportsToolAttachments: false,
	supportsMCPAttachments: false,
	supportsImageAttachments: false,
	supportsSearchResultAttachments: false,
	supportsInstructionAttachments: false,
	supportsSourceControlAttachments: false,
	supportsProblemAttachments: false,
	supportsSymbolAttachments: false,
	supportsTerminalAttachments: false,
	supportsPromptAttachments: false,
};

function contribution(): IChatSessionsExtensionPoint {
	return {
		type: WISP_AGENT_CHAT_TYPE,
		name: WISP_AGENT_NAME,
		displayName: localize('wispAgent.displayName', "Agent"),
		description: localize('wispAgent.description', "A subagent working in its own worktree on the project's host"),
		inputPlaceholder: AGENT_PLACEHOLDER,
		canDelegate: false,
		supportsDelegation: false,
		requiresCustomModels: false,
		supportsAutoModel: true,
		requiresCopilotSignIn: false,
		autoAttachReferences: false,
		capabilities: NO_ATTACHMENTS,
	};
}

function agentData(): IChatAgentData {
	return {
		id: WISP_AGENT_CHAT_TYPE,
		name: WISP_AGENT_NAME,
		fullName: localize('wispAgent.displayName', "Agent"),
		description: localize('wispAgent.description', "A subagent working in its own worktree on the project's host"),
		extensionId: new ExtensionIdentifier('wisp.agents'),
		extensionVersion: undefined,
		extensionPublisherId: 'wisp',
		extensionDisplayName: 'Wisp',
		// Upstream's chat sends nothing without a default agent, and wisp has no Copilot to be one.
		// This one is the default only for agent-mode chat, which in this window is a subagent's
		// composer: the coordinator's can't send (M4), and it answers any other chat with an error.
		isDefault: true,
		isDynamic: true,
		isCore: true,
		metadata: { themeIcon: Codicon.agent },
		slashCommands: [],
		locations: [ChatAgentLocation.Chat],
		modes: [ChatModeKind.Agent],
		disambiguation: [],
	};
}
