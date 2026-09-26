/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../base/common/event.js';
import { Disposable, DisposableStore, IDisposable, MutableDisposable } from '../../../base/common/lifecycle.js';
import { ILogger } from '../../log/common/log.js';
import { IWispdSubscribeOptions, WispdMethod, WispdState, WispdSubscriptionMessage, WispdUnavailableError } from './wispd.js';
import { IWispdClientOptions, IWispdTransportFactory, WispdClient } from './wispdClient.js';
import { WispRequests } from './wispProtocol.js';

/** The configured host: how to reach its wispd, and its `wispdTarget` key. */
export interface IWispdHost {
	readonly factory: IWispdTransportFactory;
	readonly target: string;
}

/**
 * A `WispdClient` for whichever host is configured now. `update` builds the transport factory
 * again and, when its command changed (another host, or another wispd path on it), replaces the
 * client: requests waiting on the old one fail, and live subscriptions end with `resync`, because
 * another host's `seq` numbers mean nothing here. The new client connects at once if the old one
 * had been started.
 *
 * Every state carries the `target` it is for. A window sends its own target with each request, and
 * a request is only ever sent to that target's wispd.
 */
export class WispdHostConnection extends Disposable {

	private readonly _onDidChangeState = this._register(new Emitter<WispdState>());
	readonly onDidChangeState: Event<WispdState> = this._onDidChangeState.event;

	private readonly current = this._register(new MutableDisposable<DisposableStore>());
	private client!: WispdClient;
	private command: string | undefined;
	private target!: string;
	private started = false;
	/** Ends each live subscription when the client is replaced. */
	private readonly subscriptions = new Set<() => void>();

	constructor(
		private readonly resolveHost: () => IWispdHost,
		/** Reads the settings again, for a window whose settings changed before this process noticed. */
		private readonly reloadSettings: () => Promise<void>,
		private readonly clientOptions: IWispdClientOptions,
		private readonly logger: ILogger,
	) {
		super();
		this.update();
	}

	get state(): WispdState {
		return { ...this.client.state, target: this.target };
	}

	/** Picks up a changed host. Returns whether the client was replaced. */
	update(): boolean {
		const { factory, target } = this.resolveHost();
		const retargeted = target !== this.target;
		this.target = target;
		if (factory.command === this.command) {
			if (retargeted) {
				this._onDidChangeState.fire(this.state);
			}
			return false;
		}
		if (this.command !== undefined) {
			this.logger.info(`The host changed; reconnecting through ${factory.command}`);
		}
		this.command = factory.command;

		const store = new DisposableStore();
		const client = store.add(new WispdClient(factory, this.clientOptions, this.logger));
		store.add(client.onDidChangeState(state => this._onDidChangeState.fire({ ...state, target: this.target })));
		for (const end of [...this.subscriptions]) {
			end();
		}
		this.client = client;
		this.current.value = store;
		this._onDidChangeState.fire(this.state);
		if (this.started) {
			client.start();
		}
		return true;
	}

	start(): void {
		this.started = true;
		this.client.start();
	}

	/** Starts the connection, first catching up with `target`, and returns its state. */
	async getState(target?: string): Promise<WispdState> {
		await this.catchUp(target);
		this.start();
		return this.state;
	}

	retry(): void {
		this.started = true;
		this.client.retry();
	}

	onWake(): void {
		this.client.onWake();
	}

	/**
	 * Sends a request. With `target`, the target the caller's settings name, it goes only to that
	 * target's wispd: this catches up first, and fails without sending if the settings here still
	 * name another host.
	 */
	async request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken, target?: string): Promise<WispRequests[M]['result']> {
		this.started = true;
		await this.catchUp(target);
		if (target !== undefined && target !== this.target) {
			this.logger.warn(`Not sending ${method}: the window's host is ${target}, and the settings still name ${this.target}`);
			throw new WispdUnavailableError('The host changed while the request was on its way, so it was not sent. Try again.');
		}
		return this.client.request(method, params, token);
	}

	/**
	 * A window writes `settings.json` before it sees the change itself, so when its target differs
	 * from this one, reading the file again catches up without waiting for this process's watcher.
	 */
	private async catchUp(target: string | undefined): Promise<void> {
		if (target === undefined || target === this.target) {
			return;
		}
		await this.reloadSettings();
		if (!this._store.isDisposed) {
			this.update();
		}
	}

	/** Listening subscribes, and removing the last listener unsubscribes. */
	subscribe(options: IWispdSubscribeOptions): Event<WispdSubscriptionMessage> {
		let subscription: IDisposable | undefined;
		const stop = () => {
			subscription?.dispose();
			subscription = undefined;
			this.subscriptions.delete(end);
		};
		const end = () => {
			stop();
			emitter.fire({ type: 'resync', reason: 'logIdChanged' });
		};
		const emitter = new Emitter<WispdSubscriptionMessage>({
			onWillAddFirstListener: () => {
				this.started = true;
				subscription = this.client.subscribe(options, message => emitter.fire(message));
				this.subscriptions.add(end);
			},
			onDidRemoveLastListener: stop,
		});
		return emitter.event;
	}
}
