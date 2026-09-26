/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Emitter, Event } from '../../../../../base/common/event.js';
import { Disposable, DisposableMap, IDisposable, MutableDisposable, toDisposable } from '../../../../../base/common/lifecycle.js';
import { autorun, IObservable, observableValue } from '../../../../../base/common/observable.js';
import { createDecorator } from '../../../../../platform/instantiation/common/instantiation.js';
import { ILogService } from '../../../../../platform/log/common/log.js';
import { followWispdState, IWispdService, WispdState, WispdSubscriptionMessage } from '../../../../../platform/wisp/common/wispd.js';
import type { AccountChoice, AgentRun, AgentTodoItem, LoggedEvent, LogId, ProjectId, RunId, TurnId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { generateUuidV7 } from '../../../../../platform/wisp/common/uuidv7.js';
import { applyRunState, currentStep } from '../common/wispAgentRuns.js';
import { IWispProjectsService } from './wispProjectsService.js';

export const IWispAgentsService = createDecorator<IWispAgentsService>('wispAgentsService');

/** The capability that gates every `agent/*` method and `agent.*` event (decision record 0014). */
const AGENTS_CAPABILITY = 'agents';

/** One event of a run, as the log numbers it. */
interface IWispRunEvent {
	readonly runId: RunId;
	readonly event: LoggedEvent;
}

/**
 * The connected host's agent runs, per project (decision record 0014): `agent/list {project}`,
 * then `events/subscribe {project}` from that list's `seq`. `agent.started`, `agent.updated`,
 * `agent.finished`, and `agent.diffReady` keep each run current, and every event of a run is kept
 * by `seq`, so a transcript built from `agent/events` and one built from live events agree and
 * never show an event twice. The connection replays what a subscription missed across a
 * reconnect; a `resync` lists again and fetches the missed events of every loaded transcript
 * before it subscribes, and a host change starts over.
 *
 * It is the only part of the Agents window that asks wispd about runs.
 */
export interface IWispAgentsService {
	readonly _serviceBrand: undefined;

	/** Whether the connected wispd has the `agents` capability. */
	readonly available: IObservable<boolean>;

	/** A project's runs, oldest first. Empty until the host connects and lists them. */
	runs(projectId: ProjectId): IObservable<readonly AgentRun[]>;

	getRun(runId: RunId): AgentRun | undefined;

	/** The checklist item a run is on, from its latest `todoList`. */
	step(runId: RunId): IObservable<string | undefined>;

	/**
	 * A run's events so far, oldest first. Resolves once its whole history is here: the first call
	 * pages through `agent/events`, and later calls return what is kept.
	 */
	loadEvents(runId: RunId): Promise<readonly LoggedEvent[]>;

	/** A run's events as they arrive, each once, after `loadEvents` has returned it or not. */
	readonly onDidAddEvent: Event<IWispRunEvent>;

	/**
	 * Starts a worker on a project with a new run id, and adds it without waiting for its event.
	 * Without `account`, wispd uses the worker role's default.
	 */
	start(projectId: ProjectId, prompt: string, account?: AccountChoice): Promise<AgentRun>;

	/**
	 * Sends `agent/send`. Generate `turnId` once with `generateUuidV7` and send the same params
	 * again to retry: wispd delivers the message once.
	 */
	send(runId: RunId, turnId: TurnId, text: string): Promise<AgentRun>;

	cancel(runId: RunId): Promise<AgentRun>;

	/**
	 * The text of a message this window sent to a run. wispd's log has the turn's id but not its
	 * text, so a message sent from elsewhere, or before a reload, has none.
	 */
	sentText(turnId: TurnId): string | undefined;

	/**
	 * Also follows the runs of every scope id `scopes` lists, as it follows each project's: a
	 * normal thread's run belongs to a repo entry (decision record 0017). Stops when disposed.
	 */
	addScopes(scopes: IObservable<readonly string[]>): IDisposable;

	/** Adds or updates a run another service started, such as a thread's, following its scope. */
	noteRun(run: AgentRun): void;
}

interface IRunEntry {
	readonly events: Map<number, LoggedEvent>;
	readonly step: ReturnType<typeof observableValue<string | undefined>>;
	/** The resolved history, or the fetch that is loading it. */
	history: Promise<void> | undefined;
	historyLoaded: boolean;
	/** The newest `seq` fetched through `agent/events`. */
	fetchedSeq: number;
}

/** One project's runs and its event subscription. */
class ProjectRuns extends Disposable {
	readonly runs = observableValue<readonly AgentRun[]>(this, []);
	readonly subscription = this._register(new MutableDisposable());
	/** Bumped on every list, so a stale answer or event is dropped. */
	generation = 0;
	/** `idle` until listed, and again after a list or subscription fails, so the next connect retries. */
	status: 'idle' | 'listing' | 'ready' = 'idle';
}

export class WispAgentsService extends Disposable implements IWispAgentsService {
	declare readonly _serviceBrand: undefined;

	private readonly _available = observableValue<boolean>(this, false);
	readonly available: IObservable<boolean> = this._available;

	private readonly _onDidAddEvent = this._register(new Emitter<IWispRunEvent>());
	readonly onDidAddEvent = this._onDidAddEvent.event;

	private readonly projects = this._register(new DisposableMap<ProjectId, ProjectRuns>());
	private readonly entries = new Map<RunId, IRunEntry>();
	private readonly texts = new Map<TurnId, string>();
	private readonly empty = observableValue<readonly AgentRun[]>(this, []);
	private readonly extraScopes = observableValue<readonly IObservable<readonly string[]>[]>(this, []);
	private connection: WispdState | undefined;
	/** Bumped when the host changes, so answers from the old one are dropped. */
	private hostGeneration = 0;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@ILogService private readonly logService: ILogService,
	) {
		super();
		this._register(followWispdState(wispdService, state => this.onState(state)));
		this._register(autorun(reader => {
			const projects = projectsService.projects.read(reader);
			const extra = this.extraScopes.read(reader).flatMap(scopes => scopes.read(reader));
			this.syncProjects([...new Set([...projects.map(project => project.id), ...extra])]);
		}));
	}

	runs(projectId: ProjectId): IObservable<readonly AgentRun[]> {
		return this.projects.get(projectId)?.runs ?? this.projectRuns(projectId)?.runs ?? this.empty;
	}

	getRun(runId: RunId): AgentRun | undefined {
		for (const project of this.projects.values()) {
			const run = project.runs.get().find(candidate => candidate.id === runId);
			if (run) {
				return run;
			}
		}
		return undefined;
	}

	step(runId: RunId): IObservable<string | undefined> {
		return this.entry(runId).step;
	}

	sentText(turnId: TurnId): string | undefined {
		return this.texts.get(turnId);
	}

	addScopes(scopes: IObservable<readonly string[]>): IDisposable {
		this.extraScopes.set([...this.extraScopes.get(), scopes], undefined);
		return toDisposable(() => this.extraScopes.set(this.extraScopes.get().filter(candidate => candidate !== scopes), undefined));
	}

	noteRun(run: AgentRun): void {
		this.upsertIn(this.projects.get(run.project) ?? this.track(run.project), run);
	}

	async loadEvents(runId: RunId): Promise<readonly LoggedEvent[]> {
		const entry = this.entry(runId);
		if (!entry.historyLoaded) {
			entry.history ??= this.fetchHistory(runId, entry).finally(() => entry.history = undefined);
			await entry.history;
		}
		return sortedEvents(entry);
	}

	start(projectId: ProjectId, prompt: string, account?: AccountChoice): Promise<AgentRun> {
		return this.upsertAnswer(this.wispdService.request('agent/start', { runId: generateUuidV7(), project: projectId, prompt, policy: 'workspaceWrite', ...(account ? { account } : {}) }));
	}

	send(runId: RunId, turnId: TurnId, text: string): Promise<AgentRun> {
		this.texts.set(turnId, text);
		return this.upsertAnswer(this.wispdService.request('agent/send', { runId, turnId, text }));
	}

	cancel(runId: RunId): Promise<AgentRun> {
		return this.upsertAnswer(this.wispdService.request('agent/cancel', { runId }));
	}

	/** Adds the run a request answers with, unless the host changed while it was on its way. */
	private async upsertAnswer(request: Promise<{ readonly run: AgentRun }>): Promise<AgentRun> {
		const generation = this.hostGeneration;
		const { run } = await request;
		if (generation === this.hostGeneration) {
			this.upsert(run);
		}
		return run;
	}

	private projectRuns(projectId: ProjectId): ProjectRuns | undefined {
		// Only the host's own projects and added scopes are followed; another id gets an empty list.
		const known = this.projectsService.getProject(projectId) || this.extraScopes.get().some(scopes => scopes.get().includes(projectId));
		return known ? this.track(projectId) : undefined;
	}

	private entry(runId: RunId): IRunEntry {
		let entry = this.entries.get(runId);
		if (!entry) {
			entry = { events: new Map(), step: observableValue<string | undefined>(`wispAgentStep-${runId}`, undefined), history: undefined, historyLoaded: false, fetchedSeq: 0 };
			this.entries.set(runId, entry);
		}
		return entry;
	}

	private onState(state: WispdState): void {
		const previous = this.connection;
		this.connection = state;
		if (previous !== undefined && previous.command !== state.command) {
			// Another host, or another wispd on it: its runs are not this one's.
			this.hostGeneration++;
			this.projects.clearAndDisposeAll();
			this.entries.clear();
			this.texts.clear();
		}
		if (state.kind === 'connected') {
			const available = Object.prototype.hasOwnProperty.call(state.capabilities, AGENTS_CAPABILITY);
			this._available.set(available, undefined);
			if (available) {
				// A reconnect keeps a listed project's subscription, which replays what it missed.
				this.listIdle(new Set([...this.projectsService.projects.get().map(project => project.id), ...this.extraScopes.get().flatMap(extra => extra.get())]), state.logId);
			}
		}
	}

	private syncProjects(ids: readonly ProjectId[]): void {
		const wanted = new Set(ids);
		for (const id of [...this.projects.keys()]) {
			if (!wanted.has(id)) {
				this.projects.deleteAndDispose(id);
			}
		}
		const state = this.connection;
		if (state?.kind === 'connected' && this._available.get()) {
			this.listIdle(ids, state.logId);
		}
	}

	private listIdle(ids: Iterable<ProjectId>, logId: LogId): void {
		for (const id of ids) {
			const tracked = this.track(id);
			if (tracked.status === 'idle') {
				this.list(tracked, id, logId);
			}
		}
	}

	private track(projectId: ProjectId): ProjectRuns {
		let project = this.projects.get(projectId);
		if (!project) {
			project = new ProjectRuns();
			this.projects.set(projectId, project);
		}
		return project;
	}

	private async list(project: ProjectRuns, projectId: ProjectId, logId: LogId): Promise<void> {
		const generation = ++project.generation;
		project.subscription.clear();
		project.status = 'listing';
		let listed;
		try {
			listed = await this.wispdService.request('agent/list', { project: projectId });
		} catch (error) {
			if (generation === project.generation) {
				this.logService.error(`[wisp] agent/list failed for project ${projectId}`, error);
				project.status = 'idle';
			}
			return;
		}
		if (generation !== project.generation || this.projects.get(projectId) !== project) {
			return;
		}
		// A transcript already on screen gets the events it missed before live ones arrive.
		const loaded = listed.runs.filter(run => this.entries.get(run.id)?.historyLoaded);
		await Promise.all(loaded.map(run => this.fetchHistory(run.id, this.entry(run.id)).catch(error => this.logService.error(`[wisp] agent/events failed for run ${run.id}`, error))));
		if (generation !== project.generation || this.projects.get(projectId) !== project) {
			return;
		}
		project.runs.set(listed.runs, undefined);
		project.status = 'ready';
		project.subscription.value = this.wispdService.subscribe({ after: listed.seq, project: projectId, logId })(message => {
			if (generation === project.generation) {
				this.onMessage(project, projectId, message);
			}
		});
	}

	private onMessage(project: ProjectRuns, projectId: ProjectId, message: WispdSubscriptionMessage): void {
		switch (message.type) {
			case 'event':
				this.onEvent(project, { seq: message.event.seq, time: message.event.time, project: message.event.project, event: message.event.event });
				return;
			case 'resync':
				this.logService.info(`[wisp] agent runs of ${projectId} need a resync (${message.reason}); listing again`);
				project.generation++;
				project.subscription.clear();
				if (this.connection?.kind === 'connected') {
					this.list(project, projectId, this.connection.logId);
				}
				return;
			case 'failed':
				this.logService.error(`[wisp] the agent event subscription of ${projectId} failed: ${message.message}`);
				project.generation++;
				project.status = 'idle';
				project.subscription.clear();
				return;
		}
	}

	private onEvent(project: ProjectRuns, logged: LoggedEvent): void {
		const event = logged.event;
		// A newer wispd may send kinds this editor doesn't know; they are skipped.
		if (!event.kind.startsWith('agent.') || !('runId' in event)) {
			return;
		}
		const runId = event.runId;
		switch (event.kind) {
			case 'agent.started':
				if (event.run) {
					this.upsertIn(project, event.run);
				}
				break;
			case 'agent.updated':
				this.updateIn(project, runId, run => applyRunState(run, event.state));
				break;
			case 'agent.diffReady':
				this.updateIn(project, runId, run => ({ ...run, diff: event.diff }));
				break;
			case 'agent.accountFallback':
				this.updateIn(project, runId, run => ({ ...run, accountId: event.toAccount }));
				break;
		}
		this.addEvent(runId, logged);
	}

	private addEvent(runId: RunId, logged: LoggedEvent): void {
		const entry = this.entry(runId);
		if (entry.events.has(logged.seq)) {
			return;
		}
		entry.events.set(logged.seq, logged);
		if (logged.event.kind === 'agent.output') {
			const todos = [...logged.event.items].reverse().find((item): item is { kind: 'todoList'; items: AgentTodoItem[] } => item.kind === 'todoList');
			if (todos) {
				entry.step.set(currentStep(todos.items), undefined);
			}
		}
		this._onDidAddEvent.fire({ runId, event: logged });
	}

	private async fetchHistory(runId: RunId, entry: IRunEntry): Promise<void> {
		const generation = this.hostGeneration;
		let after = entry.fetchedSeq;
		for (; ;) {
			const page = await this.wispdService.request('agent/events', { runId, after });
			if (generation !== this.hostGeneration) {
				return;
			}
			for (const logged of page.events) {
				after = Math.max(after, logged.seq);
				this.addEvent(runId, logged);
			}
			entry.fetchedSeq = after;
			if (!page.more || page.events.length === 0) {
				break;
			}
		}
		entry.historyLoaded = true;
	}

	private upsert(run: AgentRun): void {
		const project = this.projects.get(run.project) ?? this.projectRuns(run.project);
		if (project) {
			this.upsertIn(project, run);
		}
	}

	private upsertIn(project: ProjectRuns, run: AgentRun): void {
		const runs = project.runs.get();
		const index = runs.findIndex(existing => existing.id === run.id);
		if (index === -1) {
			project.runs.set([...runs, run], undefined);
		} else if (isNewer(run, runs[index])) {
			const next = [...runs];
			next[index] = run;
			project.runs.set(next, undefined);
		}
	}

	private updateIn(project: ProjectRuns, runId: RunId, update: (run: AgentRun) => AgentRun): void {
		const runs = project.runs.get();
		const index = runs.findIndex(existing => existing.id === runId);
		if (index === -1) {
			return;
		}
		const next = [...runs];
		next[index] = update(runs[index]);
		project.runs.set(next, undefined);
	}
}

/** Whether `candidate` should replace `current`: a later update, or the same time with changes. */
function isNewer(candidate: AgentRun, current: AgentRun): boolean {
	if (candidate.updatedAt !== current.updatedAt) {
		return candidate.updatedAt > current.updatedAt;
	}
	return JSON.stringify(candidate) !== JSON.stringify(current);
}

function sortedEvents(entry: IRunEntry): LoggedEvent[] {
	return [...entry.events.values()].sort((a, b) => a.seq - b.seq);
}
