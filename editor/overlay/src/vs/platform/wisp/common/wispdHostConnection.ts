/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { CancellationToken } from '../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../base/common/event.js';
import { Disposable, DisposableStore, IDisposable, MutableDisposable } from '../../../base/common/lifecycle.js';
import { ILogger } from '../../log/common/log.js';
import { IWispdSubscribeOptions, WispdMethod, WispdState, WispdSubscriptionMessage } from './wispd.js';
import { IWispdClientOptions, IWispdTransportFactory, WispdClient } from './wispdClient.js';
import { WispRequests } from './wispProtocol.js';

/**
 * A `WispdClient` for whichever host is configured now. `update` builds the transport factory
 * again and, when its command changed (another host, or another wispd path on it), replaces the
 * client: requests waiting on the old one fail, and live subscriptions end with `resync`, because
 * another host's `seq` numbers mean nothing here. The new client connects at once if the old one
 * had been started.
 */
export class WispdHostConnection extends Disposable {

	private readonly _onDidChangeState = this._register(new Emitter<WispdState>());
	readonly onDidChangeState: Event<WispdState> = this._onDidChangeState.event;

	private readonly current = this._register(new MutableDisposable<DisposableStore>());
	private client!: WispdClient;
	private command: string | undefined;
	private started = false;
	/** Ends each live subscription when the client is replaced. */
	private readonly subscriptions = new Set<() => void>();

	constructor(
		private readonly createFactory: () => IWispdTransportFactory,
		private readonly clientOptions: IWispdClientOptions,
		private readonly logger: ILogger,
	) {
		super();
		this.update();
	}

	get state(): WispdState {
		return this.client.state;
	}

	/** Picks up a changed host. Returns whether the client was replaced. */
	update(): boolean {
		const factory = this.createFactory();
		if (factory.command === this.command) {
			return false;
		}
		if (this.command !== undefined) {
			this.logger.info(`The host changed; reconnecting through ${factory.command}`);
		}
		this.command = factory.command;

		const store = new DisposableStore();
		const client = store.add(new WispdClient(factory, this.clientOptions, this.logger));
		store.add(client.onDidChangeState(state => this._onDidChangeState.fire(state)));
		for (const end of [...this.subscriptions]) {
			end();
		}
		this.client = client;
		this.current.value = store;
		this._onDidChangeState.fire(client.state);
		if (this.started) {
			client.start();
		}
		return true;
	}

	start(): void {
		this.started = true;
		this.client.start();
	}

	retry(): void {
		this.started = true;
		this.client.retry();
	}

	onWake(): void {
		this.client.onWake();
	}

	request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken): Promise<WispRequests[M]['result']> {
		this.started = true;
		return this.client.request(method, params, token);
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
