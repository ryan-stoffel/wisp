/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../base/common/event.js';
import { Disposable, IDisposable, toDisposable } from '../../../base/common/lifecycle.js';
import { IChannel, IServerChannel } from '../../../base/parts/ipc/common/ipc.js';
import { IConfigurationService } from '../../configuration/common/configuration.js';
import { createDecorator } from '../../instantiation/common/instantiation.js';
import { affectsWispdTarget, describeWispdTarget, WISP_HOST_SETTING, WISP_REMOTE_WISPD_PATH_SETTING, wispdTarget } from './wispdConfiguration.js';
import type { Capabilities, ErrorKind, EventsEventParams, JsonValue, LogId, ProjectId, ProtocolRange, WispRequests } from './wispProtocol.js';

export const WISPD_CHANNEL_NAME = 'wispd';

export const IWispdService = createDecorator<IWispdService>('wispdService');

/**
 * Why the connection is down.
 *
 * - `notStarted`: nothing has asked for the connection yet.
 * - `spawnFailed`: the `wispd` (or `ssh`) binary could not be started, for example because it is missing.
 * - `invalidHost`: `wisp.host` is not `local` and not a usable ssh destination, or
 *   `wisp.remoteWispdPath` is not a plain absolute path (decision record 0007).
 * - `unreachable`: `wispd attach` exited 4, so it never reached wispd (decision record 0010).
 * - `noRoute`: ssh could not reach the host, or the attempt timed out.
 * - `authFailed`: ssh could not authenticate. `BatchMode` turns any prompt, including one for
 *   interactive two-factor login, into this error (decision record 0007).
 * - `hostKeyUnknown`: ssh doesn't recognize the host's key yet, and `BatchMode` can't prompt to accept it.
 * - `hostKeyChanged`: the host's key doesn't match what ssh has saved for it, which can mean a
 *   possible attack, not just a reinstalled host. Never treat this the same as `hostKeyUnknown`.
 * - `wispdNotFound`: the remote shell exited 127; `wispd` isn't on the host's `PATH` or at the
 *   configured `wisp.remoteWispdPath`.
 * - `exited`: the `attach` (or `ssh`) process ended.
 * - `timedOut`: no bytes arrived for too long while a request was outstanding.
 * - `frameTooLarge`: wispd sent a line longer than `maxFrameBytes`.
 * - `protocolError`: wispd answered the handshake with something unusable, or, after the
 *   handshake, sent a line that isn't JSON-RPC (a host's shell startup files are printing to stdout).
 */
export type WispdDisconnectReason =
	| 'notStarted'
	| 'spawnFailed'
	| 'invalidHost'
	| 'unreachable'
	| 'noRoute'
	| 'authFailed'
	| 'hostKeyUnknown'
	| 'hostKeyChanged'
	| 'wispdNotFound'
	| 'exited'
	| 'timedOut'
	| 'frameTooLarge'
	| 'protocolError';

export type WispdState = WispdConnectionState & {
	/**
	 * The `wispdTarget` of the settings this state's connection was made for. The shared process
	 * sets it, so a window can tell a state for its own host from one for the host it just left.
	 */
	readonly target?: string;
};

type WispdConnectionState =
	| {
		readonly kind: 'connecting';
		/** The command that reaches wispd, such as `/Applications/Wisp.app/.../wispd attach`. */
		readonly command: string;
		/** 1 for the first attempt, counting up until a handshake succeeds. */
		readonly attempt: number;
	}
	| {
		readonly kind: 'connected';
		readonly command: string;
		/** wispd's release version. */
		readonly wispd: string;
		readonly protocol: number;
		readonly logId: LogId;
		/** What wispd supports. Hide any feature whose capability is missing. */
		readonly capabilities: Capabilities;
		readonly maxFrameBytes: number;
	}
	| {
		readonly kind: 'disconnected';
		readonly command: string;
		readonly reason: WispdDisconnectReason;
		readonly message: string;
		readonly exitCode?: number;
		/** The last lines `attach` wrote to stderr, which say why it could not reach wispd. */
		readonly stderr?: string;
		/** When the next attempt starts, in milliseconds since the epoch. */
		readonly retryAt?: number;
	}
	| {
		readonly kind: 'incompatible';
		readonly command: string;
		/** The side that has to be updated: this editor, or the wispd it reached. */
		readonly update: 'editor' | 'wispd';
		/** wispd's release version. */
		readonly wispd: string;
		/** The versions this editor speaks. */
		readonly requested: ProtocolRange;
		/** The versions wispd speaks. */
		readonly supported: ProtocolRange;
	};

/** Methods that callers may send. The client sends the handshake and manages subscriptions itself. */
export type WispdMethod = Exclude<keyof WispRequests, 'initialize' | 'events/subscribe' | 'events/unsubscribe'>;

export interface IWispdSubscribeOptions {
	/** Receive the events after this `seq`, usually the `seq` of a snapshot such as `project/list`'s. */
	readonly after: number;
	/** One project's events. Without it, host-level events such as `project.created`. */
	readonly project?: ProjectId;
	/**
	 * The `logId` that `after` belongs to, from the state when the snapshot was taken. If wispd's
	 * log has started over since, the subscription ends at once with a `resync` message.
	 */
	readonly logId?: LogId;
}

export type WispdSubscriptionMessage =
	| { readonly type: 'event'; readonly event: Omit<EventsEventParams, 'subscription'> }
	/**
	 * The subscription can't continue from its last `seq`: wispd's log started over, or the events
	 * are gone. Reload the snapshot and subscribe again from its `seq`. No more messages follow.
	 */
	| { readonly type: 'resync'; readonly reason: 'logIdChanged' | 'resyncRequired' }
	/** wispd refused the subscription, for example with `projectNotFound`. No more messages follow. */
	| { readonly type: 'failed'; readonly kind: string | undefined; readonly message: string };

/**
 * The editor's one connection to wispd (decision record 0007). It runs in the shared process, which
 * spawns `wispd attach` and speaks JSON-RPC over its stdio; windows reach it over IPC.
 *
 * The connection starts when something first asks for the state, sends a request, or subscribes.
 */
export interface IWispdService {
	readonly _serviceBrand: undefined;

	readonly onDidChangeState: Event<WispdState>;

	/**
	 * `target` is for the window's channel client, which passes its own settings' `wispdTarget`;
	 * the shared process then catches up with those settings first. Other callers leave it out.
	 */
	getState(target?: string): Promise<WispdState>;

	/**
	 * Reconnects now, without waiting for the backoff. This is the only way out of `incompatible`,
	 * so it is what a Retry button calls.
	 */
	retry(): Promise<void>;

	/**
	 * Sends a request once the connection is up, waiting up to 30 s for it.
	 *
	 * A request lost to a disconnect is sent again after the reconnect, unchanged. Methods that
	 * create or start something take the new thing's id from the caller, so generate it once with
	 * `generateUuidV7` and a resend never creates a second one.
	 *
	 * Fails with a `WispdError` when wispd answers with an error, and with a
	 * `WispdUnavailableError` when there is no connection to send it on.
	 *
	 * `target` is for the window's channel client, which passes its own settings' `wispdTarget`: the
	 * request then goes only to that host's wispd, or fails unsent. Other callers leave it out.
	 */
	request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken, target?: string): Promise<WispRequests[M]['result']>;

	/**
	 * Listening subscribes, and removing the last listener unsubscribes. Events arrive in `seq`
	 * order, each once, across reconnects.
	 */
	subscribe(options: IWispdSubscribeOptions): Event<WispdSubscriptionMessage>;
}

/** An error that wispd answered with. Match on `kind`, never on the message. */
export class WispdError extends Error {
	override readonly name = 'WispdError';

	constructor(
		readonly code: number,
		message: string,
		/** For a wisp error (-32000), its `data.kind`. A newer wispd may send kinds not listed. */
		readonly kind: ErrorKind | string | undefined,
		readonly detail?: JsonValue,
	) {
		super(message);
	}
}

/** There was no connection to send a request on, or the connection was lost too often. */
export class WispdUnavailableError extends Error {
	override readonly name = 'WispdUnavailableError';
}

type RequestOutcome =
	| { readonly type: 'result'; readonly result: unknown }
	| { readonly type: 'error'; readonly code: number; readonly message: string; readonly kind?: string; readonly detail?: JsonValue }
	| { readonly type: 'unavailable'; readonly message: string };

/** Serves an `IWispdService` to other processes. IPC keeps only an error's message, so errors travel as values. */
export class WispdChannel implements IServerChannel {

	constructor(private readonly service: IWispdService) { }

	listen<T>(_ctx: unknown, event: string, arg?: unknown): Event<T> {
		switch (event) {
			case 'onDidChangeState': return this.service.onDidChangeState as Event<T>;
			case 'subscribe': return this.service.subscribe(arg as IWispdSubscribeOptions) as Event<T>;
		}
		throw new Error(`Event not found: ${event}`);
	}

	async call<T>(_ctx: unknown, command: string, arg?: unknown, token?: CancellationToken): Promise<T> {
		switch (command) {
			case 'getState': return await this.service.getState(typeof arg === 'string' ? arg : undefined) as T;
			case 'retry': return await this.service.retry() as T;
			case 'request': {
				const [method, params, target] = arg as [WispdMethod, never, string | undefined];
				let outcome: RequestOutcome;
				try {
					outcome = { type: 'result', result: await this.service.request(method, params, token, target) };
				} catch (error) {
					if (error instanceof WispdError) {
						outcome = { type: 'error', code: error.code, message: error.message, kind: error.kind, detail: error.detail };
					} else if (error instanceof WispdUnavailableError) {
						outcome = { type: 'unavailable', message: error.message };
					} else {
						throw error;
					}
				}
				return outcome as T;
			}
		}
		throw new Error(`Call not found: ${command}`);
	}
}

/**
 * A window's `IWispdService`. The window and the shared process each read `wisp.host` on their own
 * schedule, so right after a host switch one of them is behind. This client keeps the
 * window to the host its own settings name: it sends their target with every request, which the
 * shared process refuses to send anywhere else, and it shows a state for another target as
 * `connecting` to its own.
 */
export class WispdChannelClient extends Disposable implements IWispdService {
	declare readonly _serviceBrand: undefined;

	private readonly _onDidChangeState = this._register(new Emitter<WispdState>());
	readonly onDidChangeState: Event<WispdState> = this._onDidChangeState.event;

	private target: string;
	/** The shared process's latest state, which may be for another target. */
	private remote: WispdState | undefined;
	/** Counts state events, so a slow `getState` answer can't replace a newer event. */
	private remoteEvents = 0;
	private lastFired: string | undefined;

	constructor(
		private readonly channel: IChannel,
		@IConfigurationService configurationService: IConfigurationService,
	) {
		super();
		const readTarget = () => wispdTarget(configurationService.getValue(WISP_HOST_SETTING), configurationService.getValue(WISP_REMOTE_WISPD_PATH_SETTING));
		this.target = readTarget();
		this._register(this.channel.listen<WispdState>('onDidChangeState')(state => {
			this.remoteEvents++;
			this.setRemote(state);
		}));
		this._register(configurationService.onDidChangeConfiguration(event => {
			if (!affectsWispdTarget(event)) {
				return;
			}
			const target = readTarget();
			if (target === this.target) {
				return;
			}
			this.target = target;
			if (this.remote) {
				this.fire(this.remote);
			}
			// The shared process catches up now instead of when its own file watcher fires.
			this.getState().catch(() => { /* The shared process is gone; the window is closing. */ });
		}));
	}

	async getState(): Promise<WispdState> {
		const events = this.remoteEvents;
		const state = await this.channel.call<WispdState>('getState', this.target);
		if (events === this.remoteEvents) {
			this.setRemote(state);
		}
		return this.forTarget(this.remote ?? state);
	}

	retry(): Promise<void> {
		return this.channel.call('retry');
	}

	async request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken): Promise<WispRequests[M]['result']> {
		const outcome = await this.channel.call<RequestOutcome>('request', [method, params, this.target], token);
		switch (outcome.type) {
			case 'result': return outcome.result as WispRequests[M]['result'];
			case 'error': throw new WispdError(outcome.code, outcome.message, outcome.kind, outcome.detail);
			case 'unavailable': throw new WispdUnavailableError(outcome.message);
		}
	}

	subscribe(options: IWispdSubscribeOptions): Event<WispdSubscriptionMessage> {
		return this.channel.listen('subscribe', options);
	}

	private setRemote(state: WispdState): void {
		this.remote = state;
		this.fire(state);
	}

	/** Fires the state as this window sees it, unless that is what it fired last. */
	private fire(remote: WispdState): void {
		const state = this.forTarget(remote);
		const key = JSON.stringify(state);
		if (key !== this.lastFired) {
			this.lastFired = key;
			this._onDidChangeState.fire(state);
		}
	}

	/** A state for another host means this window's host hasn't been reached yet. */
	private forTarget(state: WispdState): WispdState {
		if (state.target === undefined || state.target === this.target) {
			return state;
		}
		return { kind: 'connecting', command: describeWispdTarget(this.target), attempt: 1, target: this.target };
	}
}

/** Says what an `incompatible` state means for the user, naming the side to update. */
export function describeIncompatible(state: Extract<WispdState, { kind: 'incompatible' }>, host = 'this Mac'): string {
	const versions = `wisp speaks protocol ${formatRange(state.requested)}, and wispd ${state.wispd} on ${host} speaks ${formatRange(state.supported)}`;
	return state.update === 'wispd'
		? `Update wisp on ${host}, or restart wispd there if it is already updated: ${versions}.`
		: `Update this editor: ${versions}.`;
}

function formatRange(range: ProtocolRange): string {
	return range.min === range.max ? `${range.min}` : `${range.min} to ${range.max}`;
}

/**
 * Calls `onState` on every state change and, unless a change came first, with the state
 * `getState` answers: that answer comes from the shared process, and a slow one must not replace
 * a newer event.
 */
export function followWispdState(wispdService: IWispdService, onState: (state: WispdState) => void): IDisposable {
	let received = false;
	let disposed = false;
	const listener = wispdService.onDidChangeState(state => {
		received = true;
		onState(state);
	});
	wispdService.getState().then(state => {
		if (!received && !disposed) {
			onState(state);
		}
	}, () => { /* The shared process is gone; the window is closing. */ });
	return toDisposable(() => {
		disposed = true;
		listener.dispose();
	});
}
