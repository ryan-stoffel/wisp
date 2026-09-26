/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { toErrorMessage } from '../../../../../base/common/errorMessage.js';
import { Disposable, MutableDisposable } from '../../../../../base/common/lifecycle.js';
import { IObservable, observableValue, transaction } from '../../../../../base/common/observable.js';
import { localize } from '../../../../../nls.js';
import { createDecorator } from '../../../../../platform/instantiation/common/instantiation.js';
import { ILogService } from '../../../../../platform/log/common/log.js';
import { followWispdState, IWispdService, WispdError, WispdState, WispdSubscriptionMessage } from '../../../../../platform/wisp/common/wispd.js';
import type { LogId, Project, ProjectCreateParams } from '../../../../../platform/wisp/common/wispProtocol.js';

export const IWispProjectsService = createDecorator<IWispProjectsService>('wispProjectsService');

/**
 * Where the project list stands for the connected host.
 *
 * - `idle`: no host is connected yet, or the host changed and the new one hasn't connected.
 * - `loading`: `project/list` is on its way.
 * - `ready`: the list is the host's, and events keep it current.
 * - `failed`: wispd couldn't list its projects, for example because its store is unavailable.
 */
type WispProjectsState =
	| { readonly kind: 'idle' }
	| { readonly kind: 'loading' }
	| { readonly kind: 'ready' }
	| { readonly kind: 'failed'; readonly message: string };

/**
 * The connected host's projects, kept current from the editor's wispd connection (decision record
 * 0007): `project/list` once the host connects, then `events/subscribe` from that list's `seq`.
 * The connection replays missed events after a reconnect; a `resync` lists again, and a host change
 * starts over. It is the only part of the Agents window that asks wispd about projects.
 */
export interface IWispProjectsService {
	readonly _serviceBrand: undefined;

	/** The projects, oldest first, as `project/list` returns them. */
	readonly projects: IObservable<readonly Project[]>;

	readonly state: IObservable<WispProjectsState>;

	getProject(id: string): Project | undefined;

	/**
	 * Sends `project/create` and adds the project without waiting for its event. Generate `id` once
	 * with `generateUuidV7` and send the same params again to retry: wispd returns the project it
	 * made the first time. Fails with wispd's `WispdError`, such as `notARepository`.
	 */
	create(params: ProjectCreateParams): Promise<Project>;

	/** Lists the projects again, for a Retry after `failed`. */
	reload(): void;
}

export class WispProjectsService extends Disposable implements IWispProjectsService {
	declare readonly _serviceBrand: undefined;

	private readonly _projects = observableValue<readonly Project[]>(this, []);
	readonly projects: IObservable<readonly Project[]> = this._projects;

	private readonly _state = observableValue<WispProjectsState>(this, { kind: 'idle' });
	readonly state: IObservable<WispProjectsState> = this._state;

	private readonly subscription = this._register(new MutableDisposable());
	/** The connection's last state; its command names the host, and its log id the `seq`s. */
	private connection: WispdState | undefined;
	/** Bumped on every list and every host change, so a stale answer or event is dropped. */
	private generation = 0;
	/** Bumped on every host change only, so a list or resync doesn't drop the answer to a create. */
	private hostGeneration = 0;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@ILogService private readonly logService: ILogService,
	) {
		super();
		this._register(followWispdState(wispdService, state => this.onState(state)));
	}

	getProject(id: string): Project | undefined {
		return this._projects.get().find(project => project.id === id);
	}

	async create(params: ProjectCreateParams): Promise<Project> {
		const hostGeneration = this.hostGeneration;
		const { project } = await this.wispdService.request('project/create', params);
		if (hostGeneration === this.hostGeneration) {
			this.add(project);
		}
		return project;
	}

	reload(): void {
		if (this.connection?.kind === 'connected') {
			this.list(this.connection.logId);
		}
	}

	private onState(state: WispdState): void {
		const previous = this.connection;
		this.connection = state;
		if (previous !== undefined && previous.command !== state.command) {
			// Another host, or another wispd on it: its projects are not this one's.
			this.generation++;
			this.hostGeneration++;
			this.subscription.clear();
			transaction(tx => {
				this._projects.set([], tx);
				this._state.set({ kind: 'idle' }, tx);
			});
		}
		// A reconnect keeps the subscription, which replays what it missed; only a host that has
		// never been listed, or whose list failed, is listed now.
		const kind = this._state.get().kind;
		if (state.kind === 'connected' && (kind === 'idle' || kind === 'failed')) {
			this.list(state.logId);
		}
	}

	private async list(logId: LogId): Promise<void> {
		const generation = ++this.generation;
		this.subscription.clear();
		this._state.set({ kind: 'loading' }, undefined);
		const known = new Set(this._projects.get().map(project => project.id));
		let listed;
		try {
			listed = await this.wispdService.request('project/list', {});
		} catch (error) {
			if (generation === this.generation) {
				this.logService.error('[wisp] project/list failed', error);
				this._state.set({ kind: 'failed', message: describeFailure(error) }, undefined);
			}
			return;
		}
		if (generation !== this.generation) {
			return;
		}
		// wispd can answer a project/create sent after this list before the list itself, so the
		// list is older than a project that create added meanwhile. wispd never removes a
		// project, so that one stays, after the listed ones; its project.created event, later
		// than the list's seq, finds it already there.
		const listedIds = new Set(listed.projects.map(project => project.id));
		const newer = this._projects.get().filter(project => !known.has(project.id) && !listedIds.has(project.id));
		transaction(tx => {
			this._projects.set([...listed.projects, ...newer], tx);
			this._state.set({ kind: 'ready' }, tx);
		});
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
				if (event.kind === 'project.created') {
					this.add(event.project);
				}
				return;
			}
			case 'resync':
				this.logService.info(`[wisp] projects need a resync (${message.reason}); listing again`);
				this.generation++;
				this.subscription.clear();
				this._state.set({ kind: 'idle' }, undefined);
				if (this.connection?.kind === 'connected') {
					this.list(this.connection.logId);
				}
				return;
			case 'failed':
				this.logService.error(`[wisp] the project event subscription failed: ${message.message}`);
				this.generation++;
				this.subscription.clear();
				this._state.set({ kind: 'failed', message: message.message }, undefined);
				return;
		}
	}

	private add(project: Project): void {
		this._projects.set(upsert(this._projects.get(), project), undefined);
	}
}

/** Adds `item`, or replaces the one with its id when it differs; returns `list` itself when nothing changed. */
export function upsert<T extends { readonly id: string }>(list: readonly T[], item: T): readonly T[] {
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

function describeFailure(error: unknown): string {
	return error instanceof WispdError ? localize('wispProjects.listFailed', "wispd couldn't list its projects: {0}", error.message) : toErrorMessage(error);
}
