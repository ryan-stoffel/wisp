/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../base/common/cancellation.js';
import { CancellationError, isCancellationError } from '../../../base/common/errors.js';
import { Emitter, Event } from '../../../base/common/event.js';
import { IJsonRpcNotification, JsonRpcError, JsonRpcMessage, JsonRpcProtocol } from '../../../base/common/jsonRpcProtocol.js';
import { Disposable, DisposableStore, IDisposable, toDisposable } from '../../../base/common/lifecycle.js';
import { ILogger } from '../../log/common/log.js';
import { IWispdSubscribeOptions, WispdDisconnectReason, WispdError, WispdMethod, WispdState, WispdSubscriptionMessage, WispdUnavailableError, describeIncompatible } from './wispd.js';
import { ClientInfo, ErrorCodes, EventsEventParams, IncompatibleProtocolDetail, InitializeResult, JsonValue, LogId, PROTOCOL_VERSION, ProjectId, ProtocolRange, SubscriptionId, WispRequests } from './wispProtocol.js';

/** Why a transport closed, for the `disconnected` state. */
export interface IWispdTransportClose {
	readonly reason: WispdDisconnectReason;
	readonly message: string;
	readonly exitCode?: number;
	readonly stderr?: string;
}

/** A line-oriented connection to wispd, such as the stdio of `wispd attach`. */
export interface IWispdTransport extends IDisposable {
	/** One line from wispd, without its line ending. */
	readonly onDidReceiveLine: Event<string>;
	/** Any bytes from wispd, even part of a line. They prove the connection is alive. */
	readonly onDidReceiveData: Event<void>;
	/** Fires once, never during `create`, when the transport can't be used anymore. */
	readonly onDidClose: Event<IWispdTransportClose>;
	/** Sends one message. `line` has no line ending. */
	send(line: string): void;
}

export interface IWispdTransportFactory {
	/** What `create` runs, for the state and for messages. */
	readonly command: string;
	create(): IWispdTransport;
}

export interface IWispdTimers {
	now(): number;
	setTimeout(callback: () => void, ms: number): IDisposable;
}

const realWispdTimers: IWispdTimers = {
	now: () => Date.now(),
	setTimeout: (callback, ms) => {
		const handle = setTimeout(callback, ms);
		return toDisposable(() => clearTimeout(handle));
	},
};

export interface IWispdClientOptions {
	readonly client: ClientInfo;
	readonly timers?: IWispdTimers;
	readonly minBackoffMs?: number;
}

/** How often `host/health` goes out while connected. */
const HEARTBEAT_MS = 30_000;
/** How long an outstanding request may go without any bytes from wispd. */
const LIVENESS_MS = 10_000;
/** The same for `initialize`, which also waits for `attach` to reach or start wispd. */
const HANDSHAKE_MS = 20_000;
const MAX_BACKOFF_MS = 10_000;
/** How long a request waits for a connection before it fails. */
const CONNECT_WAIT_MS = 30_000;
/** How often a request lost to a disconnect is sent again. */
const MAX_RESENDS = 3;

/** The protocol versions this editor speaks. */
const SUPPORTED_PROTOCOL: ProtocolRange = { min: 1, max: PROTOCOL_VERSION };

class Connection extends Disposable {
	readonly protocol: JsonRpcProtocol;
	initialized: InitializeResult | undefined;
	/** Requests sent and not yet answered, which arm the liveness timer. */
	outstanding = 0;
	liveness: IDisposable | undefined;
	heartbeat: IDisposable | undefined;
	readonly subscriptions = new Map<SubscriptionId, Subscription>();
	/**
	 * wispd answers `events/subscribe` before it replays, but a replayed event can be handled
	 * before the code waiting on that answer runs. Such events wait here until it does.
	 */
	readonly early = new Map<SubscriptionId, EventsEventParams[]>();
	pendingSubscribes = 0;

	constructor(readonly transport: IWispdTransport, onNotification: (notification: IJsonRpcNotification) => void) {
		super();
		this._register(transport);
		this.protocol = this._register(new JsonRpcProtocol(message => this.write(message), { handleNotification: onNotification }));
		this._register(toDisposable(() => {
			this.liveness?.dispose();
			this.heartbeat?.dispose();
		}));
	}

	track<T extends IDisposable>(disposable: T): T {
		return this._register(disposable);
	}

	private write(message: JsonRpcMessage): void {
		this.transport.send(JSON.stringify(message));
	}
}

class Subscription {
	lastSeq: number;
	logId: LogId | undefined;
	readonly project: ProjectId | undefined;
	connection: Connection | undefined;
	serverId: SubscriptionId | undefined;
	ended = false;

	constructor(options: IWispdSubscribeOptions, readonly deliver: (message: WispdSubscriptionMessage) => void) {
		this.lastSeq = options.after;
		this.logId = options.logId;
		this.project = options.project;
	}
}

/**
 * The connection to wispd as a state machine (decision record 0007): the handshake, heartbeats,
 * reconnects with backoff, resubscribing after a reconnect, and resending lost requests. It knows
 * nothing about processes: a transport factory supplies the lines.
 */
export class WispdClient extends Disposable {

	private readonly _onDidChangeState = this._register(new Emitter<WispdState>());
	readonly onDidChangeState = this._onDidChangeState.event;

	private readonly timers: IWispdTimers;
	private readonly minBackoffMs: number;

	private _state: WispdState;
	private started = false;
	private connection: Connection | undefined;
	private attempt = 0;
	private failures = 0;
	private retryTimer: IDisposable | undefined;
	private readonly subscriptions = new Set<Subscription>();
	/** Requests waiting for a connection, failed with a cancellation when the client is disposed. */
	private readonly waiting = new Set<() => void>();

	constructor(
		private readonly factory: IWispdTransportFactory,
		private readonly options: IWispdClientOptions,
		private readonly logger: ILogger,
	) {
		super();
		this.timers = options.timers ?? realWispdTimers;
		this.minBackoffMs = options.minBackoffMs ?? 1_000;
		this._state = { kind: 'disconnected', command: factory.command, reason: 'notStarted', message: 'Not connected yet.' };

		this._register(toDisposable(() => {
			this.retryTimer?.dispose();
			const connection = this.connection;
			this.connection = undefined;
			connection?.dispose();
			this.subscriptions.clear();
			for (const cancel of [...this.waiting]) {
				cancel();
			}
		}));
	}

	get state(): WispdState {
		return this._state;
	}

	/** Connects, unless the client has already started. */
	start(): void {
		if (this.started || this._store.isDisposed) {
			return;
		}
		this.started = true;
		this.connectNow();
	}

	/** Reconnects now if the connection is down, including after `incompatible`. */
	retry(): void {
		if (this._store.isDisposed) {
			return;
		}
		this.started = true;
		if (this.connection) {
			return;
		}
		this.failures = 0;
		this.connectNow();
	}

	/** After the Mac wakes: checks a live connection at once, and skips the wait of a down one. */
	onWake(): void {
		if (!this.started || this._store.isDisposed) {
			return;
		}
		const connection = this.connection;
		if (connection?.initialized) {
			this.sendHeartbeat(connection);
		} else if (!connection && this._state.kind === 'disconnected') {
			this.failures = 0;
			this.connectNow();
		}
	}

	async request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token: CancellationToken = CancellationToken.None): Promise<WispRequests[M]['result']> {
		for (let resends = 0; ; resends++) {
			const connection = await this.whenConnected(token);
			try {
				return await this.send<WispRequests[M]['result']>(connection, method, params, token);
			} catch (error) {
				if (error instanceof JsonRpcError) {
					throw toWispdError(error);
				}
				if (!isCancellationError(error) || token.isCancellationRequested) {
					throw error;
				}
				if (resends >= MAX_RESENDS) {
					throw new WispdUnavailableError(`The connection to wispd was lost ${resends + 1} times while sending ${method}.`);
				}
				this.logger.info(`wispd: resending ${method} after a lost connection`);
			}
		}
	}

	subscribe(options: IWispdSubscribeOptions, listener: (message: WispdSubscriptionMessage) => void): IDisposable {
		const subscription = new Subscription(options, listener);
		this.subscriptions.add(subscription);
		this.start();
		const connection = this.connection;
		if (connection?.initialized) {
			this.subscribeOn(connection, subscription);
		}
		return toDisposable(() => {
			if (this.subscriptions.delete(subscription)) {
				subscription.ended = true;
				this.unsubscribeOnServer(subscription);
			}
		});
	}

	private setState(state: WispdState): void {
		this._state = state;
		this.logger.info(`wispd: ${describeState(state)}`);
		this._onDidChangeState.fire(state);
	}

	private connectNow(): void {
		this.retryTimer?.dispose();
		this.retryTimer = undefined;
		this.attempt++;
		this.setState({ kind: 'connecting', command: this.factory.command, attempt: this.attempt });

		let transport: IWispdTransport;
		try {
			transport = this.factory.create();
		} catch (error) {
			this.scheduleRetry({ reason: 'spawnFailed', message: `Could not run ${this.factory.command}: ${errorMessage(error)}` });
			return;
		}

		const connection = new Connection(transport, notification => this.onNotification(connection, notification));
		this.connection = connection;
		connection.track(transport.onDidReceiveData(() => this.armLiveness(connection)));
		connection.track(transport.onDidReceiveLine(line => this.onLine(connection, line)));
		connection.track(transport.onDidClose(close => this.drop(connection, close)));
		this.handshake(connection);
	}

	private async handshake(connection: Connection): Promise<void> {
		let result: InitializeResult;
		try {
			result = await this.send<InitializeResult>(connection, 'initialize', {
				protocol: SUPPORTED_PROTOCOL,
				client: this.options.client,
				capabilities: {},
			});
		} catch (error) {
			if (connection !== this.connection) {
				return;
			}
			if (error instanceof JsonRpcError && wispErrorKind(error) === 'incompatibleProtocol') {
				this.enterIncompatible(connection, (error.data as { detail?: IncompatibleProtocolDetail }).detail);
			} else {
				this.drop(connection, { reason: 'protocolError', message: `wispd refused the handshake: ${errorMessage(error)}` });
			}
			return;
		}
		if (connection !== this.connection) {
			return;
		}
		if (result.protocol < SUPPORTED_PROTOCOL.min || result.protocol > SUPPORTED_PROTOCOL.max) {
			this.enterIncompatible(connection, { requested: SUPPORTED_PROTOCOL, supported: { min: result.protocol, max: result.protocol }, wispd: result.wispd });
			return;
		}

		connection.initialized = result;
		this.attempt = 0;
		this.failures = 0;
		this.setState({
			kind: 'connected',
			command: this.factory.command,
			wispd: result.wispd,
			protocol: result.protocol,
			logId: result.logId,
			capabilities: result.capabilities,
			maxFrameBytes: result.maxFrameBytes,
		});
		this.scheduleHeartbeat(connection);
		for (const subscription of [...this.subscriptions]) {
			this.subscribeOn(connection, subscription);
		}
	}

	private enterIncompatible(connection: Connection, detail: IncompatibleProtocolDetail | undefined): void {
		const supported = detail?.supported ?? { min: 0, max: 0 };
		this.connection = undefined;
		this.detachSubscriptions(connection);
		this.setState({
			kind: 'incompatible',
			command: this.factory.command,
			update: supported.max < SUPPORTED_PROTOCOL.min ? 'wispd' : 'editor',
			wispd: detail?.wispd ?? 'unknown',
			requested: SUPPORTED_PROTOCOL,
			supported,
		});
		connection.dispose();
	}

	/** Closes a connection and, unless the client is incompatible or disposed, schedules the next attempt. */
	private drop(connection: Connection, close: IWispdTransportClose): void {
		if (connection !== this.connection) {
			return;
		}
		this.connection = undefined;
		this.detachSubscriptions(connection);
		this.scheduleRetry(close);
		connection.dispose();
	}

	private scheduleRetry(close: IWispdTransportClose): void {
		if (this._store.isDisposed) {
			return;
		}
		const delay = Math.min(MAX_BACKOFF_MS, this.minBackoffMs * 2 ** Math.min(this.failures, 20));
		this.failures++;
		this.setState({
			kind: 'disconnected',
			command: this.factory.command,
			reason: close.reason,
			message: close.message,
			exitCode: close.exitCode,
			stderr: close.stderr,
			retryAt: this.timers.now() + delay,
		});
		this.retryTimer?.dispose();
		this.retryTimer = this.timers.setTimeout(() => this.connectNow(), delay);
	}

	private detachSubscriptions(connection: Connection): void {
		for (const subscription of this.subscriptions) {
			if (subscription.connection === connection) {
				subscription.connection = undefined;
				subscription.serverId = undefined;
			}
		}
	}

	private whenConnected(token: CancellationToken): Promise<Connection> {
		this.start();
		if (token.isCancellationRequested) {
			return Promise.reject(new CancellationError());
		}
		const ready = this.connection?.initialized ? this.connection : undefined;
		if (ready) {
			return Promise.resolve(ready);
		}
		if (this._state.kind === 'incompatible') {
			return Promise.reject(new WispdUnavailableError(describeIncompatible(this._state)));
		}

		if (this._store.isDisposed) {
			return Promise.reject(new CancellationError());
		}

		return new Promise<Connection>((resolve, reject) => {
			const store = new DisposableStore();
			const settle = (fn: () => void) => {
				this.waiting.delete(cancel);
				store.dispose();
				fn();
			};
			const cancel = () => settle(() => reject(new CancellationError()));
			this.waiting.add(cancel);
			store.add(this.onDidChangeState(state => {
				const connection = this.connection;
				if (state.kind === 'connected' && connection) {
					settle(() => resolve(connection));
				} else if (state.kind === 'incompatible') {
					settle(() => reject(new WispdUnavailableError(describeIncompatible(state))));
				}
			}));
			store.add(this.timers.setTimeout(() => {
				const state = this._state;
				const why = state.kind === 'disconnected' ? `: ${state.message}` : '';
				settle(() => reject(new WispdUnavailableError(`wispd is not reachable through ${this.factory.command}${why}`)));
			}, CONNECT_WAIT_MS));
			store.add(token.onCancellationRequested(cancel));
		});
	}

	private send<T>(connection: Connection, method: keyof WispRequests, params: unknown, token: CancellationToken = CancellationToken.None): Promise<T> {
		// Only received bytes restart the liveness timer; sending never does (decision record 0007).
		if (connection.outstanding++ === 0) {
			this.armLiveness(connection);
		}
		const request = connection.protocol.sendRequest<T>({ method, params }, token, id => {
			connection.protocol.sendNotification({ method: '$/cancelRequest', params: { id } });
		});
		return request.finally(() => {
			if (--connection.outstanding === 0) {
				connection.liveness?.dispose();
				connection.liveness = undefined;
			}
		});
	}

	/** Restarts the liveness timer while a request is outstanding, and stops it otherwise. */
	private armLiveness(connection: Connection): void {
		connection.liveness?.dispose();
		connection.liveness = undefined;
		if (connection !== this.connection || connection.outstanding === 0) {
			return;
		}
		const ms = connection.initialized ? LIVENESS_MS : HANDSHAKE_MS;
		connection.liveness = this.timers.setTimeout(() => {
			this.drop(connection, { reason: 'timedOut', message: `wispd sent nothing for ${ms / 1000} s while a request was outstanding.` });
		}, ms);
	}

	private scheduleHeartbeat(connection: Connection): void {
		connection.heartbeat?.dispose();
		connection.heartbeat = this.timers.setTimeout(() => this.sendHeartbeat(connection), HEARTBEAT_MS);
	}

	private sendHeartbeat(connection: Connection): void {
		if (connection !== this.connection || !connection.initialized) {
			return;
		}
		this.scheduleHeartbeat(connection);
		this.send(connection, 'host/health', {}).catch(() => { /* a lost connection is handled by drop */ });
	}

	private onLine(connection: Connection, line: string): void {
		if (connection !== this.connection || line.trim() === '') {
			return;
		}
		let message: unknown;
		try {
			message = JSON.parse(line);
		} catch {
			this.onNonJsonLine(connection, `not JSON (${line.length} characters)`);
			return;
		}
		if (typeof message !== 'object' || message === null) {
			this.onNonJsonLine(connection, 'not a JSON-RPC message');
			return;
		}
		connection.protocol.handleMessage(message as JsonRpcMessage).catch(error => this.logger.error(`wispd: ${errorMessage(error)}`));
	}

	/**
	 * Before `initialize` answers, a host's shell startup files (over SSH, `~/.zshenv`) can print
	 * lines that are not JSON; those are skipped and logged (decision record 0007). After that, a
	 * non-JSON line means the protocol stream is corrupted, so the connection is dropped.
	 */
	private onNonJsonLine(connection: Connection, reason: string): void {
		if (!connection.initialized) {
			this.logger.warn(`wispd: skipped a line that is ${reason}`);
			return;
		}
		this.drop(connection, {
			reason: 'protocolError',
			message: `wispd sent a line that is ${reason}. Quiet the host's shell startup files so only wisp's protocol reaches stdout.`,
		});
	}

	private onNotification(connection: Connection, notification: IJsonRpcNotification): void {
		if (notification.method !== 'events/event' || connection !== this.connection) {
			return;
		}
		const params = notification.params as EventsEventParams;
		const subscription = connection.subscriptions.get(params.subscription);
		if (subscription) {
			this.deliver(subscription, params);
		} else if (connection.pendingSubscribes > 0) {
			const early = connection.early.get(params.subscription) ?? [];
			early.push(params);
			connection.early.set(params.subscription, early);
		}
	}

	private deliver(subscription: Subscription, params: EventsEventParams): void {
		if (subscription.ended || params.seq <= subscription.lastSeq) {
			return;
		}
		subscription.lastSeq = params.seq;
		const { subscription: _serverId, ...event } = params;
		subscription.deliver({ type: 'event', event });
	}

	private async subscribeOn(connection: Connection, subscription: Subscription): Promise<void> {
		const logId = connection.initialized?.logId;
		if (subscription.ended || subscription.connection === connection || logId === undefined) {
			return;
		}
		if (subscription.logId !== undefined && subscription.logId !== logId) {
			this.end(subscription, { type: 'resync', reason: 'logIdChanged' });
			return;
		}
		subscription.logId = logId;
		subscription.connection = connection;
		connection.pendingSubscribes++;
		try {
			const params = subscription.project === undefined
				? { after: subscription.lastSeq }
				: { after: subscription.lastSeq, project: subscription.project };
			const { subscription: serverId } = await this.send<WispRequests['events/subscribe']['result']>(connection, 'events/subscribe', params);
			if (connection !== this.connection || subscription.connection !== connection) {
				return;
			}
			if (subscription.ended) {
				this.send(connection, 'events/unsubscribe', { subscription: serverId }).catch(() => { /* best effort */ });
				return;
			}
			subscription.serverId = serverId;
			connection.subscriptions.set(serverId, subscription);
			const early = connection.early.get(serverId);
			connection.early.delete(serverId);
			for (const params of early ?? []) {
				this.deliver(subscription, params);
			}
		} catch (error) {
			if (error instanceof JsonRpcError) {
				const kind = wispErrorKind(error);
				this.end(subscription, kind === 'resyncRequired'
					? { type: 'resync', reason: 'resyncRequired' }
					: { type: 'failed', kind, message: error.message });
			}
		} finally {
			connection.pendingSubscribes--;
			if (connection.pendingSubscribes === 0) {
				connection.early.clear();
			}
		}
	}

	private end(subscription: Subscription, message: WispdSubscriptionMessage): void {
		if (subscription.ended) {
			return;
		}
		subscription.ended = true;
		this.subscriptions.delete(subscription);
		this.unsubscribeOnServer(subscription);
		subscription.deliver(message);
	}

	private unsubscribeOnServer(subscription: Subscription): void {
		const connection = subscription.connection;
		const serverId = subscription.serverId;
		subscription.connection = undefined;
		subscription.serverId = undefined;
		if (connection && serverId !== undefined && connection === this.connection) {
			connection.subscriptions.delete(serverId);
			this.send(connection, 'events/unsubscribe', { subscription: serverId }).catch(() => { /* best effort */ });
		}
	}
}

function wispErrorKind(error: JsonRpcError): string | undefined {
	if (error.code !== ErrorCodes.WispError || typeof error.data !== 'object' || error.data === null) {
		return undefined;
	}
	const kind = (error.data as { kind?: unknown }).kind;
	return typeof kind === 'string' ? kind : undefined;
}

function toWispdError(error: JsonRpcError): WispdError {
	const detail = typeof error.data === 'object' && error.data !== null ? (error.data as { detail?: JsonValue }).detail : undefined;
	return new WispdError(error.code, error.message, wispErrorKind(error), detail);
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function describeState(state: WispdState): string {
	switch (state.kind) {
		case 'connecting':
			return `connecting through ${state.command} (attempt ${state.attempt})`;
		case 'connected':
			return `connected to wispd ${state.wispd}, protocol ${state.protocol}, log ${state.logId}`;
		case 'disconnected':
			return `disconnected (${state.reason}): ${state.message}`;
		case 'incompatible':
			return `incompatible: ${describeIncompatible(state)}`;
	}
}
