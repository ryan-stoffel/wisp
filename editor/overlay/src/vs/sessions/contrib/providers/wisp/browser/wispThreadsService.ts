/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Disposable, MutableDisposable } from '../../../../../base/common/lifecycle.js';
import { derived, IObservable, observableValue, transaction } from '../../../../../base/common/observable.js';
import { createDecorator } from '../../../../../platform/instantiation/common/instantiation.js';
import { ILogService } from '../../../../../platform/log/common/log.js';
import { generateUuidV7 } from '../../../../../platform/wisp/common/uuidv7.js';
import { IWispdService, WispdState, WispdSubscriptionMessage } from '../../../../../platform/wisp/common/wispd.js';
import type { AccountChoice, AgentRun, LogId, Repo, RepoId, RunId, Thread } from '../../../../../platform/wisp/common/wispProtocol.js';
import { THREADS_CAPABILITY } from '../common/wispThreads.js';
import { IWispAgentsService } from './wispAgentsService.js';

export const IWispThreadsService = createDecorator<IWispThreadsService>('wispThreadsService');

/**
 * The connected host's normal threads and their repo entries (decision record 0017):
 * `thread/list` once the host connects, then host-level events from that list's `seq`. Each
 * entry's runs come from `IWispAgentsService`, which this service asks to follow every entry, so a
 * thread's transcript, messages, and Stop work as a subagent's do (0015).
 */
export interface IWispThreadsService {
	readonly _serviceBrand: undefined;

	/** Whether the connected wispd has the `threads` capability. */
	readonly available: IObservable<boolean>;

	/** Every repo entry, oldest first, including wispd's scratch entry once it exists. */
	readonly repos: IObservable<readonly Repo[]>;

	/** Every thread, oldest first. */
	readonly threads: IObservable<readonly Thread[]>;

	getRepo(id: RepoId): Repo | undefined;

	/** Registers the repository at a host path, or returns the entry it already has. */
	addRepo(path: string): Promise<Repo>;

	/**
	 * Starts a thread as `runId` in a repo entry, or with no repo. Generate `runId` once with
	 * `generateUuidV7` and send the same params again to retry.
	 */
	start(runId: RunId, repo: RepoId | undefined, prompt: string, account?: AccountChoice): Promise<{ readonly thread: Thread; readonly run: AgentRun }>;

	setArchived(runId: RunId, archived: boolean): Promise<Thread>;

	/** Deletes a thread, stopping its agent first if it runs. */
	delete(runId: RunId): Promise<void>;
}

export class WispThreadsService extends Disposable implements IWispThreadsService {
	declare readonly _serviceBrand: undefined;

	private readonly _available = observableValue<boolean>(this, false);
	readonly available: IObservable<boolean> = this._available;
	private readonly _repos = observableValue<readonly Repo[]>(this, []);
	readonly repos: IObservable<readonly Repo[]> = this._repos;
	private readonly _threads = observableValue<readonly Thread[]>(this, []);
	readonly threads: IObservable<readonly Thread[]> = this._threads;

	private readonly subscription = this._register(new MutableDisposable());
	private connection: WispdState | undefined;
	private status: 'idle' | 'listing' | 'ready' = 'idle';
	/** Bumped on every list and every host change, so a stale answer or event is dropped. */
	private generation = 0;
	private receivedState = false;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@IWispAgentsService private readonly agentsService: IWispAgentsService,
		@ILogService private readonly logService: ILogService,
	) {
		super();
		// Every entry's runs, and those of a thread whose entry hasn't arrived yet.
		this._register(agentsService.addScopes(derived(this, reader => [...new Set([
			...this._repos.read(reader).map(repo => repo.id),
			...this._threads.read(reader).map(thread => thread.repo),
		])])));
		this._register(wispdService.onDidChangeState(state => {
			this.receivedState = true;
			this.onState(state);
		}));
		wispdService.getState().then(state => {
			if (!this.receivedState && !this._store.isDisposed) {
				this.onState(state);
			}
		}, () => { /* The shared process is gone; the window is closing. */ });
	}

	getRepo(id: RepoId): Repo | undefined {
		return this._repos.get().find(repo => repo.id === id);
	}

	async addRepo(path: string): Promise<Repo> {
		const generation = this.generation;
		const { repo } = await this.wispdService.request('repo/add', { id: generateUuidV7(), path });
		if (generation === this.generation) {
			this.upsertRepo(repo);
		}
		return repo;
	}

	async start(runId: RunId, repo: RepoId | undefined, prompt: string, account?: AccountChoice): Promise<{ readonly thread: Thread; readonly run: AgentRun }> {
		const generation = this.generation;
		const started = await this.wispdService.request('thread/start', { runId, prompt, ...(repo ? { repo } : {}), ...(account ? { account } : {}) });
		if (generation === this.generation) {
			this.upsertThread(started.thread);
			this.agentsService.noteRun(started.run);
		}
		return started;
	}

	async setArchived(runId: RunId, archived: boolean): Promise<Thread> {
		const generation = this.generation;
		const { thread } = await this.wispdService.request('thread/archive', { runId, archived });
		if (generation === this.generation) {
			this.upsertThread(thread);
		}
		return thread;
	}

	async delete(runId: RunId): Promise<void> {
		const generation = this.generation;
		// wispd stops a running agent first, and answers once it has exited.
		await this.wispdService.request('thread/delete', { runId });
		if (generation === this.generation) {
			this.removeThread(runId);
		}
	}

	private onState(state: WispdState): void {
		const previous = this.connection;
		this.connection = state;
		if (previous !== undefined && previous.command !== state.command) {
			this.generation++;
			this.status = 'idle';
			this.subscription.clear();
			transaction(tx => {
				this._repos.set([], tx);
				this._threads.set([], tx);
			});
		}
		if (state.kind !== 'connected') {
			return;
		}
		const available = Object.prototype.hasOwnProperty.call(state.capabilities, THREADS_CAPABILITY);
		this._available.set(available, undefined);
		if (available && this.status === 'idle') {
			this.list(state.logId);
		}
	}

	private async list(logId: LogId): Promise<void> {
		const generation = ++this.generation;
		this.subscription.clear();
		this.status = 'listing';
		let listed;
		try {
			listed = await this.wispdService.request('thread/list', {});
		} catch (error) {
			if (generation === this.generation) {
				this.logService.error('[wisp] thread/list failed', error);
				this.status = 'idle';
			}
			return;
		}
		if (generation !== this.generation) {
			return;
		}
		transaction(tx => {
			this._repos.set(listed.repos, tx);
			this._threads.set(listed.threads, tx);
		});
		this.status = 'ready';
		this.subscription.value = this.wispdService.subscribe({ after: listed.seq, logId })(message => {
			if (generation === this.generation) {
				this.onMessage(message);
			}
		});
	}

	private onMessage(message: WispdSubscriptionMessage): void {
		switch (message.type) {
			case 'event': {
				const event = message.event.event;
				// A newer wispd may send kinds this editor doesn't know; they are skipped.
				switch (event.kind) {
					case 'repo.added':
						this.upsertRepo(event.repo);
						break;
					case 'thread.started':
					case 'thread.updated':
						this.upsertThread(event.thread);
						break;
					case 'thread.deleted':
						this.removeThread(event.runId);
						break;
				}
				return;
			}
			case 'resync':
				this.logService.info(`[wisp] threads need a resync (${message.reason}); listing again`);
				this.generation++;
				this.subscription.clear();
				this.status = 'idle';
				if (this.connection?.kind === 'connected') {
					this.list(this.connection.logId);
				}
				return;
			case 'failed':
				this.logService.error(`[wisp] the thread event subscription failed: ${message.message}`);
				this.generation++;
				this.subscription.clear();
				this.status = 'idle';
				return;
		}
	}

	private upsertRepo(repo: Repo): void {
		this._repos.set(upsert(this._repos.get(), repo), undefined);
	}

	private upsertThread(thread: Thread): void {
		this._threads.set(upsert(this._threads.get(), thread), undefined);
	}

	private removeThread(runId: RunId): void {
		const threads = this._threads.get();
		if (threads.some(thread => thread.id === runId)) {
			this._threads.set(threads.filter(thread => thread.id !== runId), undefined);
		}
	}
}

function upsert<T extends { readonly id: string }>(list: readonly T[], item: T): readonly T[] {
	const index = list.findIndex(existing => existing.id === item.id);
	if (index === -1) {
		return [...list, item];
	}
	if (JSON.stringify(list[index]) === JSON.stringify(item)) {
		return list;
	}
	const next = [...list];
	next[index] = item;
	return next;
}
