/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { Disposable, DisposableMap, MutableDisposable } from '../../../../base/common/lifecycle.js';
import { autorun, IObservable, observableValue } from '../../../../base/common/observable.js';
import { localize } from '../../../../nls.js';
import { createDecorator } from '../../../../platform/instantiation/common/instantiation.js';
import { ILogService } from '../../../../platform/log/common/log.js';
import { generateUuidV7 } from '../../../../platform/wisp/common/uuidv7.js';
import { IWispdService, WispdError, WispdState, WispdSubscriptionMessage } from '../../../../platform/wisp/common/wispd.js';
import type { ContextFile, ContextReadResult, ProjectId } from '../../../../platform/wisp/common/wispProtocol.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { WISP_CONTEXT_EDITOR_WRITER } from '../common/wispContextUri.js';

export const IWispContextService = createDecorator<IWispContextService>('wispContextService');

/**
 * One project's shared context, as the Project tab shows it (docs/design/agents-window.md, note
 * 10). `idle`, `loading`, and `failed` mirror {@link WispProjectsState}.
 */
export type WispContextState =
	| { readonly kind: 'idle' }
	| { readonly kind: 'loading' }
	| { readonly kind: 'ready'; readonly files: readonly ContextFile[] }
	| { readonly kind: 'failed'; readonly message: string };

/**
 * A project's shared context (decision record 0005), kept current from the editor's wispd
 * connection: `context/list` once watched and the host is connected, then `events/subscribe`
 * scoped to the project for `context.changed`. `read` and `write` are plain passthroughs to
 * `context/read` and `context/write`, used directly by the `wisp-context:` file system provider
 * so opening a file works even for a project whose list nobody has asked for yet.
 */
export interface IWispContextService {
	readonly _serviceBrand: undefined;

	/**
	 * The shared context for one project, live. The first call for a project starts watching it
	 * (listing it once the host is connected, and keeping it current after); later calls for the
	 * same project share that watch.
	 */
	state(project: ProjectId): IObservable<WispContextState>;

	/** Lists the project's shared context again, for a Retry after `failed`. */
	reload(project: ProjectId): void;

	/** Reads one file's content. Fails with wispd's `contextNotFound` if it does not exist. */
	read(project: ProjectId, path: string): Promise<ContextReadResult>;

	/** Writes one file in full, attributed to the editor, and folds the result into any watch. */
	write(project: ProjectId, path: string, content: string): Promise<ContextFile>;
}

/** One project's watch, kept for as long as the service lives once created: `WispContextService`
 * pauses it (clears the subscription, resets to `idle`) rather than disposing it when the project
 * leaves `IWispProjectsService.projects`, so a provider `watch()` still holding this same {@link
 * Watch.state} keeps working if the project reappears (a host switched away and back). Only the
 * whole service being disposed disposes a `Watch`. */
class Watch extends Disposable {
	readonly state = observableValue<WispContextState>('wispContextState', { kind: 'idle' });
	readonly subscription = this._register(new MutableDisposable());
	/** Bumped on every list, resync, and host change, so a stale answer or event is dropped. */
	generation = 0;
	/** Whether something has asked for this project's state, so a host connecting should list it. */
	watching = false;

	/** Whether this watch, or the service, has since been disposed: a `load` in flight when either
	 * happens must not hand `subscription` a value, since `MutableDisposable` silently drops (never
	 * disposes) one set after it is disposed. */
	get disposed(): boolean {
		return this._store.isDisposed;
	}
}

export class WispContextService extends Disposable implements IWispContextService {
	declare readonly _serviceBrand: undefined;

	private readonly watches = this._register(new DisposableMap<ProjectId, Watch>());
	private connection: WispdState | undefined;
	/** The state arrives from the shared process; a slow first answer must not overwrite a newer event. */
	private receivedState = false;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@ILogService private readonly logService: ILogService,
	) {
		super();
		this._register(wispdService.onDidChangeState(state => {
			this.receivedState = true;
			this.onState(state);
		}));
		wispdService.getState().then(state => {
			if (!this.receivedState && !this._store.isDisposed) {
				this.onState(state);
			}
		}, () => { /* The shared process is gone; the window is closing. */ });
		// Only once the current host's projects are confidently known (never mid reset, such as the
		// moment a host change clears the list before relisting) does a project missing from it mean
		// it is gone, not just not listed yet. Absent projects are paused (subscription cleared,
		// state reset to idle) rather than removed, and a project that comes back is reloaded here
		// if something is still watching it, so a `wisp-context:` editor left open across the gap
		// picks its changes back up instead of going quiet forever.
		this._register(autorun(reader => {
			const projects = projectsService.projects.read(reader);
			if (projectsService.state.read(reader).kind !== 'ready') {
				return;
			}
			const ids = new Set(projects.map(project => project.id));
			for (const [id, watch] of this.watches) {
				const present = ids.has(id);
				const kind = watch.state.get().kind;
				if (!present && kind !== 'idle') {
					watch.generation++;
					watch.subscription.clear();
					watch.state.set({ kind: 'idle' }, undefined);
				} else if (present && watch.watching && kind === 'idle' && this.connection?.kind === 'connected') {
					this.load(id, watch);
				}
			}
		}));
	}

	state(project: ProjectId): IObservable<WispContextState> {
		const watch = this.ensure(project);
		if (!watch.watching) {
			watch.watching = true;
			if (this.connection?.kind === 'connected') {
				this.load(project, watch);
			}
		}
		return watch.state;
	}

	reload(project: ProjectId): void {
		const watch = this.watches.get(project);
		if (watch && this.connection?.kind === 'connected') {
			this.load(project, watch);
		}
	}

	async read(project: ProjectId, path: string): Promise<ContextReadResult> {
		return this.wispdService.request('context/read', { project, path });
	}

	async write(project: ProjectId, path: string, content: string): Promise<ContextFile> {
		const { file } = await this.wispdService.request('context/write', {
			id: generateUuidV7(),
			project,
			path,
			content,
			writer: WISP_CONTEXT_EDITOR_WRITER,
		});
		const watch = this.watches.get(project);
		if (watch) {
			this.upsert(watch, file);
		}
		return file;
	}

	private ensure(project: ProjectId): Watch {
		let watch = this.watches.get(project);
		if (!watch) {
			watch = new Watch();
			this.watches.set(project, watch);
		}
		return watch;
	}

	private onState(state: WispdState): void {
		const previous = this.connection;
		this.connection = state;
		if (previous !== undefined && previous.command !== state.command) {
			// Another host, or another wispd on it: nothing watched so far is this one's.
			for (const watch of this.watches.values()) {
				watch.generation++;
				watch.subscription.clear();
				watch.state.set({ kind: 'idle' }, undefined);
			}
		}
		if (state.kind !== 'connected') {
			return;
		}
		// A paused watch's project may no longer exist at all (#106's third review): reloading it here
		// on every reconnect or host switch, the way an idle-or-failed watch for a still-current project
		// needs, would ask wispd about a project it may since have dropped entirely. Only reload one
		// this host's own list still has; the constructor's autorun is what resumes a paused watch once
		// a project it names comes back, for the case where the list itself is what changes.
		if (this.projectsService.state.get().kind !== 'ready') {
			return;
		}
		const ids = new Set(this.projectsService.projects.get().map(project => project.id));
		for (const [project, watch] of this.watches) {
			const kind = watch.state.get().kind;
			if (watch.watching && ids.has(project) && (kind === 'idle' || kind === 'failed')) {
				this.load(project, watch);
			}
		}
	}

	/**
	 * `context/list` has no `seq` of its own to subscribe from (unlike `project/list`), so this
	 * borrows `project/list`'s: it reflects the log's current head regardless of what it lists.
	 * Fetching it first, then the files, means a write racing the two is covered by the
	 * subscription's replay if it lands after the files snapshot, and by the snapshot if it lands
	 * before; either way `upsert` treats it as one more idempotent update. #214 tracks giving
	 * `context/list` its own `seq`, which would drop the extra round trip.
	 */
	private async load(project: ProjectId, watch: Watch): Promise<void> {
		const generation = ++watch.generation;
		watch.subscription.clear();
		watch.state.set({ kind: 'loading' }, undefined);
		let seq: number;
		try {
			({ seq } = await this.wispdService.request('project/list', {}));
		} catch (error) {
			if (generation === watch.generation && !watch.disposed) {
				this.fail(project, watch, error);
			}
			return;
		}
		let files: readonly ContextFile[];
		try {
			({ files } = await this.wispdService.request('context/list', { project }));
		} catch (error) {
			if (generation === watch.generation && !watch.disposed) {
				this.fail(project, watch, error);
			}
			return;
		}
		if (generation !== watch.generation || watch.disposed) {
			// Disposed either because this project left the list mid-load, or because the whole
			// service did (the window closing): `subscription.value` below would silently drop the
			// new subscription rather than dispose it, since `MutableDisposable` no-ops once disposed.
			return;
		}
		watch.state.set({ kind: 'ready', files }, undefined);
		const connection = this.connection;
		const logId = connection?.kind === 'connected' ? connection.logId : undefined;
		watch.subscription.value = this.wispdService.subscribe({ after: seq, project, logId })(message => {
			if (generation === watch.generation) {
				this.onMessage(project, watch, message);
			}
		});
	}

	private onMessage(project: ProjectId, watch: Watch, message: WispdSubscriptionMessage): void {
		switch (message.type) {
			case 'event': {
				const event = message.event.event;
				// A newer wispd may send kinds this editor doesn't know; they are skipped.
				if (event.kind === 'context.changed') {
					this.upsert(watch, event.file);
				}
				return;
			}
			case 'resync':
				this.logService.info(`[wisp] shared context for ${project} needs a resync (${message.reason}); listing again`);
				watch.generation++;
				watch.subscription.clear();
				watch.state.set({ kind: 'idle' }, undefined);
				if (this.connection?.kind === 'connected') {
					this.load(project, watch);
				}
				return;
			case 'failed':
				this.logService.error(`[wisp] the shared context subscription for ${project} failed: ${message.message}`);
				watch.generation++;
				watch.subscription.clear();
				watch.state.set({ kind: 'failed', message: message.message }, undefined);
				return;
		}
	}

	/**
	 * Folds `file` into a `ready` watch. Anything else (`idle`, `loading`, `failed`) is left alone:
	 * treating it as an empty list would turn it into a `ready` one holding only this file, hiding a
	 * `failed` state's error, or racing the in-flight `load` that is about to replace it anyway.
	 */
	private upsert(watch: Watch, file: ContextFile): void {
		const state = watch.state.get();
		if (state.kind !== 'ready') {
			return;
		}
		const files = state.files;
		const index = files.findIndex(existing => existing.path === file.path);
		if (index !== -1 && JSON.stringify(files[index]) === JSON.stringify(file)) {
			return;
		}
		const next = index === -1 ? [...files, file] : files.map((existing, i) => i === index ? file : existing);
		next.sort((a, b) => a.path.localeCompare(b.path));
		watch.state.set({ kind: 'ready', files: next }, undefined);
	}

	private fail(project: ProjectId, watch: Watch, error: unknown): void {
		this.logService.error(`[wisp] listing shared context for ${project} failed`, error);
		watch.state.set({ kind: 'failed', message: describeFailure(error) }, undefined);
	}
}

function describeFailure(error: unknown): string {
	if (error instanceof WispdError) {
		return localize('wispContext.listFailed', "wispd couldn't list the shared context: {0}", error.message);
	}
	return toErrorMessage(error);
}
