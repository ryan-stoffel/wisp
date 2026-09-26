/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { renderAsPlaintext } from '../../../../../base/browser/markdownRenderer.js';
import { mainWindow } from '../../../../../base/browser/window.js';
import { CancellationTokenSource } from '../../../../../base/common/cancellation.js';
import { Codicon } from '../../../../../base/common/codicons.js';
import { Disposable, IDisposable } from '../../../../../base/common/lifecycle.js';
import { constObservable, observableValue } from '../../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../../base/common/themables.js';
import { URI } from '../../../../../base/common/uri.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { IContextViewDelegate, IContextViewService } from '../../../../../platform/contextview/browser/contextView.js';
import type { AgentEventsParams, AgentRun, AgentSendParams, AgentStartParams, LoggedEvent, WispEvent } from '../../../../../platform/wisp/common/wispProtocol.js';
import type { IChatPillSection } from '../../../../../workbench/browser/chatPills.js';
import { IChatProgress } from '../../../../../workbench/contrib/chat/common/chatService/chatService.js';
import { IChatSessionServerRequest, IChatSessionsService } from '../../../../../workbench/contrib/chat/common/chatSessionsService.js';
import { IChatAgentImplementation, IChatAgentRequest, IChatAgentService } from '../../../../../workbench/contrib/chat/common/participants/chatAgents.js';
import { ISessionsService } from '../../../../services/sessions/browser/sessionsService.js';
import { ChatInteractivity, ChatOriginKind, SessionStatus } from '../../../../services/sessions/common/session.js';
import { IActiveSession } from '../../../../services/sessions/common/sessionsManagement.js';
import { SessionBackgroundActivitiesControl } from '../../../chat/browser/sessionBackgroundActivitiesControl.js';
import { WispAgentChat } from '../../../providers/wisp/browser/wispAgentChat.js';
import { WispAgentChatSession, WispAgentChatSessions } from '../../../providers/wisp/browser/wispAgentChatSessions.js';
import { WispAgentTranscript } from '../../../providers/wisp/browser/wispAgentTranscript.js';
import { WispProjectSession } from '../../../providers/wisp/browser/wispProjectSession.js';
import { agentChatResource, agentRunOf, agentState, agentTitle, applyRunState } from '../../../providers/wisp/common/wispAgentRuns.js';
import { projectResource } from '../../../providers/wisp/common/wispProjects.js';
import { AGENTS_PANEL_ROWS, agentsSummary, WispAgentsPanel, WispAgentStatusAnnouncer } from '../../browser/wispAgentsPanel.js';
import { agentsWindowServices, IAgentsWindowServices, settle } from './wispAgentsTestServices.js';
import { connected, project, SSH_COMMAND } from './wispHostTestUtils.js';

const PROJECT = '0192f0c4-0000-7000-8000-000000000001';
const RUN = '0192f0c4-0000-7000-8000-0000000000aa';
const TURN = '0192f0c4-0000-7000-8000-0000000000bb';
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function run(options: Partial<AgentRun> = {}): AgentRun {
	return {
		id: RUN,
		project: PROJECT,
		prompt: 'Render invoice PDFs\nfrom Stripe line items',
		policy: 'workspaceWrite',
		status: 'running',
		backend: 'claude',
		accountId: 'claude',
		createdAt: '2026-09-25T10:00:00Z',
		updatedAt: '2026-09-25T10:00:00Z',
		...options,
	};
}

function logged(seq: number, event: WispEvent, time = '2026-09-25T10:00:01Z'): LoggedEvent {
	return { seq, time, project: PROJECT, event };
}

/** A worker's first turn as wispd logs it (recorded from wispd with the smoke check's fake claude). */
function firstTurn(runId = RUN): LoggedEvent[] {
	return [
		logged(10, { kind: 'agent.output', runId, items: [{ kind: 'turnStarted' }, { kind: 'sessionStarted', sessionId: 's-1', model: 'claude-fake' }] }),
		logged(11, {
			kind: 'agent.output', runId, items: [
				{ kind: 'toolCall', callId: 'toolu_1', name: 'Write', input: { file_path: '/wt/FAKE_AGENT_NOTES.md', content: 'Notes.' } },
				{ kind: 'toolResult', callId: 'toolu_1', status: 'ok', output: 'File created successfully.' },
				{ kind: 'text', messageId: 'msg_1', text: 'Fake agent wrote FAKE_AGENT_NOTES.md.' },
				{ kind: 'usage', model: 'claude-fake', inputTokens: 1, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0 },
				{ kind: 'turnFinished', result: 'Fake agent wrote FAKE_AGENT_NOTES.md.' },
			],
		}, '2026-09-25T10:00:03Z'),
		logged(12, { kind: 'agent.finished', runId, outcome: { status: 'completed', result: 'done' } }, '2026-09-25T10:00:04Z'),
		logged(13, { kind: 'agent.diffReady', runId, diff: { commit: 'abc', files: 1, insertions: 1, deletions: 0 } }, '2026-09-25T10:00:04Z'),
		logged(14, { kind: 'agent.updated', runId, state: { status: 'completed', accountId: 'claude', sessionId: 's-1', diff: { commit: 'abc', files: 1, insertions: 1, deletions: 0 }, updatedAt: '2026-09-25T10:00:04Z' } }, '2026-09-25T10:00:04Z'),
	];
}

/** A message sent with `agent/send`, resuming the finished session. */
function followUp(turnId = TURN): LoggedEvent[] {
	return [
		logged(20, { kind: 'agent.updated', runId: RUN, state: { status: 'running', accountId: 'claude', sessionId: 's-1', updatedAt: '2026-09-25T10:01:00Z' } }, '2026-09-25T10:01:00Z'),
		logged(21, { kind: 'agent.output', runId: RUN, items: [{ kind: 'turnStarted', turnId }, { kind: 'sessionStarted', sessionId: 's-1' }] }, '2026-09-25T10:01:00Z'),
		logged(22, { kind: 'agent.output', runId: RUN, items: [{ kind: 'text', text: 'Fake agent heard: also add tests' }, { kind: 'turnFinished', turnId }] }, '2026-09-25T10:01:02Z'),
		logged(23, { kind: 'agent.finished', runId: RUN, outcome: { status: 'completed' } }, '2026-09-25T10:01:03Z'),
		logged(24, { kind: 'agent.updated', runId: RUN, state: { status: 'completed', accountId: 'claude', sessionId: 's-1', updatedAt: '2026-09-25T10:01:03Z' } }, '2026-09-25T10:01:03Z'),
	];
}

function connectedWithAgents(command?: string) {
	return { ...connected(command), capabilities: { agents: {} } };
}

interface IAgentsServices extends IAgentsWindowServices {
	readonly listed: AgentRun[];
	readonly history: Map<string, LoggedEvent[]>;
	/** Sends an event to the project's agent subscription. */
	emit(event: LoggedEvent): void;
	resync(): void;
}

suite('wisp: agents', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	/** Services whose wispd has one project, lists `listed` at `seq` 9, and pages `history` two at a time. */
	function services(listed: AgentRun[] = [run()], host = 'local'): IAgentsServices {
		const context = agentsWindowServices(disposables, true, host);
		const history = new Map<string, LoggedEvent[]>();
		context.wispd.handler = async (method, params) => {
			switch (method) {
				case 'project/list':
					return { projects: [project(PROJECT, 'billing')], seq: 5 };
				case 'agent/list':
					return { runs: listed, seq: 9 };
				case 'agent/events': {
					const { runId, after } = params as AgentEventsParams;
					const events = (history.get(runId) ?? []).filter(event => event.seq > after);
					return { events: events.slice(0, 2), more: events.length > 2 };
				}
				case 'agent/start': {
					const { runId, project: projectId, prompt } = params as AgentStartParams;
					return { run: run({ id: runId, project: projectId, prompt, status: 'starting' }) };
				}
				case 'agent/send':
				case 'agent/cancel':
					return { run: run({ id: (params as AgentSendParams).runId, updatedAt: '2026-09-25T10:05:00Z' }) };
			}
			throw new Error(`unexpected ${method}`);
		};
		const agentSubscription = () => {
			const entry = [...context.wispd.subscriptions].reverse().find(candidate => candidate.active && candidate.options.project === PROJECT);
			if (!entry) {
				throw new Error('no agent subscription');
			}
			return entry;
		};
		return {
			...context,
			listed,
			history,
			emit: event => agentSubscription().emitter.fire({ type: 'event', event }),
			resync: () => agentSubscription().emitter.fire({ type: 'resync', reason: 'resyncRequired' }),
		};
	}

	async function connectedServices(listed?: AgentRun[], host?: string): Promise<IAgentsServices> {
		const context = services(listed, host);
		context.wispd.setState(connectedWithAgents());
		await settle();
		return context;
	}

	suite('runs', () => {

		test('each status reads as a chat status, a mark with its own shape, and text', () => {
			const read = (options: Partial<AgentRun>, step?: string) => {
				const state = agentState(run(options), step);
				return [state.status, state.mark, state.label, state.running];
			};
			assert.deepStrictEqual(read({ status: 'starting' }), [SessionStatus.InProgress, 'hollow', 'Starting', true]);
			assert.deepStrictEqual(read({ status: 'running' }), [SessionStatus.InProgress, 'running', 'Running', true]);
			assert.deepStrictEqual(read({ status: 'running' }, 'Writing tests'), [SessionStatus.InProgress, 'running', 'Writing tests', true]);
			assert.deepStrictEqual(read({ status: 'completed', diff: { commit: 'a', files: 2, insertions: 3, deletions: 1 } }), [SessionStatus.NeedsInput, 'filled', 'Needs review', false]);
			assert.deepStrictEqual(read({ status: 'completed' }), [SessionStatus.Completed, 'done', 'Done', false]);
			assert.deepStrictEqual(read({ status: 'failed' }), [SessionStatus.Error, 'diamond', 'Failed', false]);
			assert.deepStrictEqual(read({ status: 'cancelled' }), [SessionStatus.Completed, 'square', 'Stopped', false]);
			assert.deepStrictEqual(read({ status: 'interrupted' }), [SessionStatus.Completed, 'square', 'Interrupted', false]);
			assert.deepStrictEqual(read({ status: 'paused' as AgentRun['status'] }), [SessionStatus.Completed, 'square', 'paused', false], 'a status a newer wispd sends reads as itself');
		});

		test('a title is the prompt\'s first line, and a chat resource names its project and run', () => {
			assert.strictEqual(agentTitle('\n  Render invoice PDFs  \nand more'), 'Render invoice PDFs');
			assert.strictEqual(agentTitle('x'.repeat(100)).length, 80);
			assert.strictEqual(agentTitle('   '), 'Agent');
			const resource = agentChatResource(PROJECT, RUN);
			assert.deepStrictEqual(agentRunOf(resource), { projectId: PROJECT, runId: RUN });
			assert.strictEqual(agentRunOf(projectResource(PROJECT)), undefined);
		});

		test('agent.updated replaces the changing fields, and clears an error once it runs again', () => {
			const failed = run({ status: 'failed', error: 'boom' });
			const next = applyRunState(failed, { status: 'running', accountId: 'key-1', updatedAt: '2026-09-25T11:00:00Z' });
			assert.deepStrictEqual([next.status, next.accountId, next.error, next.prompt], ['running', 'key-1', undefined, failed.prompt]);
		});
	});

	suite('service', () => {

		test('lists a project\'s runs once connected, then subscribes to the project from that seq', async () => {
			const { wispd, agents } = await connectedServices();
			assert.deepStrictEqual(agents.runs(PROJECT).get().map(candidate => candidate.id), [RUN]);
			assert.ok(agents.available.get());
			assert.deepStrictEqual(wispd.requests.filter(([method]) => method === 'agent/list'), [['agent/list', { project: PROJECT }]]);
			assert.deepStrictEqual(wispd.subscriptions.filter(entry => entry.options.project).map(entry => entry.options), [{ after: 9, project: PROJECT, logId: 'log-1' }]);
		});

		test('a wispd without the agents capability is never asked', async () => {
			const { wispd, agents } = services();
			wispd.setState(connected());
			await settle();
			assert.strictEqual(agents.available.get(), false);
			assert.deepStrictEqual(wispd.requests.filter(([method]) => method.startsWith('agent/')), []);
			assert.deepStrictEqual(agents.runs(PROJECT).get(), []);
		});

		test('events keep a run current, and each seq counts once', async () => {
			const context = await connectedServices([]);
			const added: number[] = [];
			disposables.add(context.agents.onDidAddEvent(({ event }) => added.push(event.seq)));
			context.emit(logged(10, { kind: 'agent.started', runId: RUN, run: run({ status: 'starting' }) }));
			assert.deepStrictEqual(context.agents.runs(PROJECT).get().map(candidate => candidate.status), ['starting']);
			context.emit(logged(11, { kind: 'agent.updated', runId: RUN, state: { status: 'running', accountId: 'claude', updatedAt: '2026-09-25T10:00:01Z' } }));
			context.emit(logged(11, { kind: 'agent.updated', runId: RUN, state: { status: 'running', accountId: 'claude', updatedAt: '2026-09-25T10:00:01Z' } }));
			context.emit(logged(12, { kind: 'agent.output', runId: RUN, items: [{ kind: 'todoList', items: [{ text: 'Read the code', status: 'completed' }, { text: 'Writing tests', status: 'inProgress' }] }] }));
			context.emit(logged(13, { kind: 'agent.diffReady', runId: RUN, diff: { commit: 'abc', files: 3, insertions: 10, deletions: 2 } }));
			context.emit(logged(14, { kind: 'project.created', project: project('other') }));
			const current = context.agents.getRun(RUN)!;
			assert.deepStrictEqual([current.status, current.diff?.files], ['running', 3]);
			assert.strictEqual(context.agents.step(RUN).get(), 'Writing tests');
			assert.deepStrictEqual(added, [10, 11, 12, 13]);
		});

		test('a transcript loads through agent/events, a page at a time, and merges with live events by seq', async () => {
			const context = await connectedServices();
			context.history.set(RUN, firstTurn());
			context.emit(firstTurn()[3]);
			const events = await context.agents.loadEvents(RUN);
			assert.deepStrictEqual(events.map(event => event.seq), [10, 11, 12, 13, 14]);
			assert.deepStrictEqual(context.wispd.requests.filter(([method]) => method === 'agent/events').map(([, params]) => (params as AgentEventsParams).after), [0, 11, 13]);
			await context.agents.loadEvents(RUN);
			assert.strictEqual(context.wispd.requests.filter(([method]) => method === 'agent/events').length, 3, 'a loaded transcript is kept');
		});

		test('a resync lists again and fetches what a loaded transcript missed before it subscribes', async () => {
			const context = await connectedServices();
			context.history.set(RUN, firstTurn().slice(0, 2));
			await context.agents.loadEvents(RUN);
			context.history.set(RUN, firstTurn());
			context.wispd.requests.length = 0;
			context.resync();
			await settle();
			assert.deepStrictEqual(context.wispd.requests.map(([method, params]) => method === 'agent/events' ? `${method} ${(params as AgentEventsParams).after}` : method), ['agent/list', 'agent/events 11', 'agent/events 13']);
			assert.deepStrictEqual((await context.agents.loadEvents(RUN)).map(event => event.seq), [10, 11, 12, 13, 14]);
			assert.strictEqual(context.wispd.subscriptions.filter(entry => entry.active && entry.options.project).length, 1);
		});

		test('start, send, and cancel go to wispd, with a new run id and the sent text kept', async () => {
			const { wispd, agents } = await connectedServices([]);
			const started = await agents.start(PROJECT, 'Fix rounding', { kind: 'subscription', backend: 'claude' });
			const [, startParams] = wispd.requests.find(([method]) => method === 'agent/start')!;
			assert.match((startParams as AgentStartParams).runId, UUID_V7);
			assert.deepStrictEqual({ ...(startParams as AgentStartParams), runId: '' }, { runId: '', project: PROJECT, prompt: 'Fix rounding', policy: 'workspaceWrite', account: { kind: 'subscription', backend: 'claude' } });
			assert.deepStrictEqual(agents.runs(PROJECT).get().map(candidate => candidate.id), [started.id], 'the run shows without waiting for its event');

			await agents.send(started.id, TURN, 'also add tests');
			assert.strictEqual(agents.sentText(TURN), 'also add tests');
			await agents.cancel(started.id);
			assert.deepStrictEqual(wispd.requests.filter(([method]) => method === 'agent/send' || method === 'agent/cancel'), [
				['agent/send', { runId: started.id, turnId: TURN, text: 'also add tests' }],
				['agent/cancel', { runId: started.id }],
			]);
		});

		test('another host starts over', async () => {
			const { wispd, agents } = await connectedServices();
			assert.strictEqual(agents.runs(PROJECT).get().length, 1);
			wispd.setState(connectedWithAgents(SSH_COMMAND));
			await settle();
			assert.strictEqual(agents.getRun(RUN)?.id, RUN, 'the new host lists its own runs');
			assert.strictEqual(wispd.requests.filter(([method]) => method === 'agent/list').length, 2);
		});
	});

	suite('chats', () => {

		test('each run is a tool-origin chat of its project, and the sidebar still lists only the project', async () => {
			const { provider } = await connectedServices([run(), run({ id: 'r2', prompt: 'Fix rounding', status: 'failed', createdAt: '2026-09-25T10:01:00Z' })]);
			const sessions = provider.getSessions();
			assert.strictEqual(sessions.length, 1, 'subagents are chats, not sessions');
			const session = sessions[0] as WispProjectSession;
			const [coordinator, ...subagents] = session.chats.get();
			assert.strictEqual(session.mainChat.get(), coordinator);
			assert.deepStrictEqual(subagents.map(chat => [
				chat.resource.toString(),
				chat.title.get(),
				chat.status.get(),
				chat.origin?.kind,
				chat.origin?.parentChat?.toString(),
				chat.interactivity.get(),
				renderAsPlaintext(chat.description.get()!),
			]), [
				[agentChatResource(PROJECT, RUN).toString(), 'Render invoice PDFs', SessionStatus.InProgress, ChatOriginKind.Tool, coordinator.resource.toString(), ChatInteractivity.Full, 'this Mac'],
				[agentChatResource(PROJECT, 'r2').toString(), 'Fix rounding', SessionStatus.Error, ChatOriginKind.Tool, coordinator.resource.toString(), ChatInteractivity.Full, 'this Mac'],
			]);
			assert.strictEqual(session.chats.get()[1], subagents[0], 'a chat keeps its identity');
		});

		test('a chat follows its run, and names a remote host where it runs', async () => {
			const context = await connectedServices([run()], 'mac-mini');
			context.wispd.setState(connectedWithAgents(SSH_COMMAND));
			await settle();
			const session = context.provider.getSessions()[0] as WispProjectSession;
			const chat = session.getAgentChat(RUN) as WispAgentChat;
			assert.strictEqual(renderAsPlaintext(chat.description.get()!), 'mac-mini');
			context.emit(logged(30, { kind: 'agent.updated', runId: RUN, state: { status: 'completed', accountId: 'claude', diff: { commit: 'a', files: 1, insertions: 1, deletions: 0 }, updatedAt: '2026-09-25T10:09:00Z' } }));
			assert.deepStrictEqual([chat.status.get(), chat.state.get().label], [SessionStatus.NeedsInput, 'Needs review']);
			assert.strictEqual(session.getAgentChat(RUN), chat);
		});
	});

	suite('transcript', () => {

		function transcript(sentText: (turnId: string) => string | undefined = () => undefined, canReview = true): WispAgentTranscript {
			return new WispAgentTranscript(run(), { sentText, canReview: () => canReview });
		}

		function describe(parts: readonly IChatProgress[]): string[] {
			return parts.map(part => {
				switch (part.kind) {
					case 'markdownContent': return `text: ${part.content.value}`;
					case 'toolInvocationSerialized': return `tool: ${typeof part.pastTenseMessage === 'string' ? part.pastTenseMessage : part.pastTenseMessage?.value}`;
					case 'info': return `info: ${renderAsPlaintext(part.content)}`;
					case 'warning': return `warning: ${renderAsPlaintext(part.content)}`;
					case 'command': return `buttons: ${[part.command, ...part.additionalCommands ?? []].map(command => command.title).join(', ')}`;
					default: return part.kind;
				}
			});
		}

		test('the task\'s turn shows tool calls and text, and closes with a card once the commit is reported', () => {
			const built = transcript();
			const events = firstTurn();
			for (const event of events.slice(0, 3)) {
				built.accept(event);
			}
			assert.strictEqual(built.turns.length, 1);
			assert.strictEqual(built.current.prompt, run().prompt);
			assert.strictEqual(built.current.complete, false, 'the response stays open until the run settles');
			assert.deepStrictEqual(describe(built.current.parts), ['tool: Write FAKE_AGENT_NOTES.md', 'text: Fake agent wrote FAKE_AGENT_NOTES.md.']);

			const changes = [...built.accept(events[3]), ...built.accept(events[4])];
			assert.deepStrictEqual(describe(built.current.parts).slice(2), ['info: Finished. 1 file changed, +1 -0.', 'buttons: Review changes']);
			assert.deepStrictEqual(changes.map(change => change.kind), ['parts', 'turnCompleted']);
			assert.strictEqual(built.current.complete, true);
			assert.strictEqual(built.current.completedAt, Date.parse('2026-09-25T10:00:03Z'), 'it finished when wispd said the turn did');
			assert.deepStrictEqual(built.accept(events[4]), [], 'an event applies once');
		});

		test('without #157\'s command the card offers no review, and a failure offers Retry', () => {
			const built = transcript(undefined, false);
			for (const event of firstTurn().slice(0, 2)) {
				built.accept(event);
			}
			built.accept(logged(12, { kind: 'agent.finished', runId: RUN, outcome: { status: 'failed', failure: 'commitFailed', message: 'no git identity' } }));
			built.accept(logged(13, { kind: 'agent.updated', runId: RUN, state: { status: 'failed', accountId: 'claude', error: 'no git identity', updatedAt: '2026-09-25T10:00:04Z' } }));
			assert.deepStrictEqual(describe(built.current.parts).slice(2), ['warning: Failed: no git identity', 'buttons: Retry']);
		});

		test('a message starts a turn with the text this window sent, or a placeholder', () => {
			const built = transcript(turnId => turnId === TURN ? 'also add tests' : undefined);
			for (const event of [...firstTurn(), ...followUp()]) {
				built.accept(event);
			}
			assert.deepStrictEqual(built.turns.map(turn => [turn.id, turn.turnId, turn.prompt, turn.complete]), [
				[`task-${RUN}`, undefined, run().prompt, true],
				[TURN, TURN, 'also add tests', true],
			]);
			assert.deepStrictEqual(describe(built.turns[1].parts), ['text: Fake agent heard: also add tests', 'info: Finished. 1 file changed, +1 -0.', 'buttons: Review changes'], 'the diff is the run\'s, against its worktree\'s base');

			const other = transcript();
			for (const event of [...firstTurn(), ...followUp('0192f0c4-0000-7000-8000-0000000000cc')]) {
				other.accept(event);
			}
			assert.strictEqual(other.turns[1].prompt, 'A message sent to this agent');
		});

		test('a dropped message is reported, and a run that stops without finishing closes', () => {
			const built = transcript();
			built.accept(firstTurn()[0]);
			const dropped = built.accept(logged(11, { kind: 'agent.output', runId: RUN, items: [{ kind: 'followUpDropped', turnId: TURN }] }));
			assert.deepStrictEqual(dropped.map(change => change.kind === 'dropped' ? `dropped ${change.turnId}` : change.kind), ['parts', `dropped ${TURN}`]);
			built.accept(logged(12, { kind: 'agent.output', runId: RUN, items: [{ kind: 'toolCall', callId: 'c', name: 'Bash', input: { command: 'npm test' } }] }));
			const closed = built.completeAll(undefined);
			assert.deepStrictEqual(closed.map(change => change.kind), ['parts', 'turnCompleted']);
			assert.deepStrictEqual(describe(built.current.parts).at(-1), 'tool: Bash npm test', 'a call with no result shows when its turn ends');
		});
	});

	suite('chat session', () => {

		function chatSession(events: LoggedEvent[], current: AgentRun, cancels: string[] = []): WispAgentChatSession {
			const built = new WispAgentTranscript(current, { sentText: () => 'also add tests', canReview: () => true });
			for (const event of events) {
				built.accept(event);
			}
			return disposables.add(new WispAgentChatSession(agentChatResource(PROJECT, RUN), built, constObservable(current), async () => cancels.push(RUN)));
		}

		test('a running turn streams through progressObs, and Stop cancels the run', async () => {
			const cancels: string[] = [];
			const session = chatSession(firstTurn().slice(0, 2), run(), cancels);
			assert.deepStrictEqual(session.history.map(item => item.type), ['request'], 'the running turn is not history yet');
			assert.strictEqual(session.isCompleteObs.get(), false);
			assert.strictEqual(session.progressObs.get().length, 2);
			session.accept(firstTurn()[2]);
			session.accept(firstTurn()[3]);
			session.accept(firstTurn()[4]);
			assert.strictEqual(session.progressObs.get().length, 4);
			assert.strictEqual(session.isCompleteObs.get(), true);
			assert.strictEqual(await session.interruptActiveResponseCallback(), true);
			assert.deepStrictEqual(cancels, [RUN]);
		});

		test('a turn this window didn\'t send starts a server request; one it sent streams to its request', async () => {
			const session = chatSession(firstTurn(), run({ status: 'completed' }));
			assert.deepStrictEqual(session.history.map(item => item.type), ['request', 'response']);
			const started: IChatSessionServerRequest[] = [];
			disposables.add(session.onDidStartServerRequest(request => started.push(request)));

			const sent: IChatProgress[][] = [];
			const token = disposables.add(new CancellationTokenSource());
			const expected = session.expect(TURN, parts => sent.push(parts), token.token);
			for (const event of followUp()) {
				session.accept(event);
			}
			await expected.done;
			expected.dispose();
			assert.strictEqual(started.length, 0, 'the sent message\'s turn goes to its own request');
			assert.deepStrictEqual(sent.flat().map(part => part.kind), ['markdownContent', 'info', 'command']);

			for (const event of followUp('0192f0c4-0000-7000-8000-0000000000cc').map(event => ({ ...event, seq: event.seq + 10 }))) {
				session.accept(event);
			}
			assert.deepStrictEqual(started.map(request => [request.id, request.prompt]), [['0192f0c4-0000-7000-8000-0000000000cc', 'also add tests']]);
			assert.strictEqual(session.isCompleteObs.get(), true);
		});

		test('the chat agent sends agent/send with a new turn id and returns when the turn closes', async () => {
			const context = await connectedServices([run({ status: 'completed' })]);
			context.history.set(RUN, firstTurn());
			let agent: IChatAgentImplementation | undefined;
			context.instantiationService.stub(IChatSessionsService, { registerChatSessionContribution: () => Disposable.None, registerChatSessionContentProvider: () => Disposable.None } as unknown as IChatSessionsService);
			context.instantiationService.stub(IChatAgentService, { registerDynamicAgent: (_data: unknown, impl: IChatAgentImplementation) => { agent = impl; return Disposable.None; } } as unknown as IChatAgentService);
			const sessions = disposables.add(context.instantiationService.createInstance(WispAgentChatSessions));
			const session = await sessions.provideChatSessionContent(agentChatResource(PROJECT, RUN), new CancellationTokenSource().token);
			disposables.add(session);
			assert.strictEqual(session.title, 'Render invoice PDFs');
			assert.deepStrictEqual(session.history.map(item => item.type), ['request', 'response']);

			const progress: IChatProgress[] = [];
			const token = disposables.add(new CancellationTokenSource());
			const result = agent!.invoke({ sessionResource: agentChatResource(PROJECT, RUN), message: 'also add tests' } as IChatAgentRequest, parts => progress.push(...parts), [], token.token);
			await settle();
			const [, params] = context.wispd.requests.find(([method]) => method === 'agent/send')!;
			const turnId = (params as AgentSendParams).turnId;
			assert.match(turnId, UUID_V7);
			assert.deepStrictEqual(params, { runId: RUN, turnId, text: 'also add tests' });
			for (const event of followUp(turnId)) {
				context.emit(event);
			}
			assert.deepStrictEqual(await result, {});
			assert.deepStrictEqual(progress.map(part => part.kind), ['markdownContent', 'info']);
			assert.strictEqual(progress.some(part => part.kind === 'command'), false, 'without #157, the card offers no review');
		});
	});

	suite('pill and panel', () => {

		function openedChats(): { sessionsService: ISessionsService; opened: URI[] } {
			const opened: URI[] = [];
			return { opened, sessionsService: { openChat: async (_session: unknown, chat: URI) => { opened.push(chat); } } as unknown as ISessionsService };
		}

		test('the pill lists the coordinator\'s subagents newest first, as Agents, with status icons and where they run', async () => {
			const context = await connectedServices([run(), run({ id: 'r2', prompt: 'Fix rounding', status: 'failed' })]);
			const session = context.provider.getSessions()[0] as WispProjectSession;
			const { sessionsService, opened } = openedChats();
			const control = disposables.add(new SessionBackgroundActivitiesControl(constObservable(session as unknown as IActiveSession), session.mainChat, constObservable(true), constObservable(true), sessionsService));
			const [section] = control.sections.get();
			assert.strictEqual(section.title, 'Agents');
			assert.deepStrictEqual(section.entries.map(entry => [entry.label, entry.icon && ThemeIcon.asClassName(entry.icon), entry.badge]), [
				['Fix rounding', ThemeIcon.asClassName(Codicon.error), 'this Mac'],
				['Render invoice PDFs', ThemeIcon.asClassName(ThemeIcon.modify(Codicon.loading, 'spin')), 'this Mac'],
			]);
			section.entries[1].open();
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [agentChatResource(PROJECT, RUN).toString()]);
		});

		test('the pill reads Agents and its count, and names how many run', () => {
			assert.deepStrictEqual(agentsSummary([{ state: agentState(run()) }, { state: agentState(run({ status: 'failed' })) }]), { count: 2, running: 1, ariaLabel: 'Agents, 2, 1 running' });
		});

		test('the panel shows five rows, then More, moves with the arrows, opens with Enter, and closes with Escape', async () => {
			const runs = Array.from({ length: 7 }, (_, index) => run({ id: `r${index}`, prompt: `Task ${index}`, status: index === 0 ? 'failed' : 'running' }));
			const context = await connectedServices(runs);
			const opened: string[] = [];
			const sections = observableValue<readonly IChatPillSection[]>('sections', [{
				title: 'Agents',
				entries: runs.map(candidate => ({ id: agentChatResource(PROJECT, candidate.id).toString(), label: candidate.prompt, open: () => opened.push(candidate.id) })),
			}]);
			const contextView = new TestContextViewService();
			const panel = new WispAgentsPanel(context.agents, context.hostStatus, contextView as unknown as IContextViewService);
			const trigger = mainWindow.document.body.appendChild(mainWindow.document.createElement('button'));
			let hidden = 0;
			try {
				const shown = panel.show(trigger, sections, () => hidden++);
				const element = contextView.container!.querySelector<HTMLElement>('.wisp-agents-panel')!;
				assert.strictEqual(element.getAttribute('role'), 'dialog');
				const list = element.querySelector<HTMLElement>('.wisp-agents-panel-list')!;
				assert.strictEqual(list.getAttribute('role'), 'listbox');
				const rows = () => [...list.querySelectorAll<HTMLElement>('.wisp-agents-panel-row')];
				assert.strictEqual(rows().length, AGENTS_PANEL_ROWS);
				assert.deepStrictEqual(rows().slice(0, 2).map(row => row.getAttribute('aria-label')), ['Task 0, Failed, on this Mac', 'Task 1, Running, on this Mac']);
				assert.ok(rows()[0].querySelector('.wisp-agent-mark-diamond'), 'a failure has its own shape');
				const more = element.querySelector<HTMLButtonElement>('.wisp-agents-panel-more')!;
				assert.strictEqual(more.hidden, false);

				const key = (target: HTMLElement, name: string) => target.dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true }));
				assert.strictEqual(list.getAttribute('aria-activedescendant'), rows()[0].id);
				key(list, 'ArrowDown');
				assert.strictEqual(list.getAttribute('aria-activedescendant'), rows()[1].id);
				more.click();
				assert.strictEqual(rows().length, 7);
				assert.strictEqual(more.hidden, true);
				key(list, 'End');
				key(list, 'Enter');
				assert.deepStrictEqual(opened, ['r6']);
				assert.strictEqual(hidden, 1, 'opening a row closes the panel');

				panel.show(trigger, sections, () => hidden++);
				key(contextView.container!.querySelector<HTMLElement>('.wisp-agents-panel-list')!, 'Escape');
				assert.strictEqual(hidden, 2);
				shown.dispose();
			} finally {
				trigger.remove();
				contextView.hideContextView();
			}
		});

		test('a state change is announced once, without opening the panel', async () => {
			const context = await connectedServices();
			const announced: string[] = [];
			disposables.add(new WispAgentStatusAnnouncer(context.agents, message => announced.push(message)));
			context.emit(logged(30, { kind: 'agent.updated', runId: RUN, state: { status: 'running', accountId: 'claude', updatedAt: '2026-09-25T10:00:05Z' } }));
			context.emit(logged(31, { kind: 'agent.updated', runId: RUN, state: { status: 'failed', accountId: 'claude', error: 'x', updatedAt: '2026-09-25T10:00:06Z' } }));
			context.emit(logged(32, { kind: 'agent.updated', runId: RUN, state: { status: 'failed', accountId: 'claude', error: 'x', updatedAt: '2026-09-25T10:00:07Z' } }));
			assert.deepStrictEqual(announced, ['Render invoice PDFs: Failed']);
		});
	});
});

/** Renders a context view into a detached container at once, as the real one does into its layer. */
class TestContextViewService {
	container: HTMLElement | undefined;
	private delegate: IContextViewDelegate | undefined;
	private rendered: IDisposable | undefined;

	showContextView(delegate: IContextViewDelegate): { close: () => void } {
		this.hideContextView();
		this.delegate = delegate;
		this.container = mainWindow.document.body.appendChild(mainWindow.document.createElement('div'));
		this.rendered = delegate.render(this.container);
		return { close: () => this.hideContextView() };
	}

	hideContextView(): void {
		const delegate = this.delegate;
		if (!delegate) {
			return;
		}
		this.delegate = undefined;
		this.rendered?.dispose();
		this.container?.remove();
		delegate.onHide?.();
	}
}
