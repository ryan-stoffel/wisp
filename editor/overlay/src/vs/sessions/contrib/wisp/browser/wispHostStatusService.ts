/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Disposable } from '../../../../base/common/lifecycle.js';
import { IObservable, observableValue } from '../../../../base/common/observable.js';
import { localize } from '../../../../nls.js';
import { IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { createDecorator } from '../../../../platform/instantiation/common/instantiation.js';
import { IWispdService, WispdState } from '../../../../platform/wisp/common/wispd.js';
import { hostLabel, isLocalHost, WISP_HOST_SETTING } from '../../../../platform/wisp/common/wispdConfiguration.js';
import { describeHostStatus, IWispHostStatus, WispHostProblem } from './wispHostStatus.js';

export const IWispHostStatusService = createDecorator<IWispHostStatusService>('wispHostStatusService');

/** The host's connection status in the Agents window, as the sidebar chip and the no-host view show it. */
export interface IWispHostStatusService {
	readonly _serviceBrand: undefined;

	readonly status: IObservable<IWispHostStatus>;

	/** The raw connection state behind `status`. */
	readonly state: IObservable<WispdState>;

	/** `wisp.host` as it is set: `local`, or an ssh destination. */
	readonly configuredHost: IObservable<string>;

	/** Reconnects now, without waiting for the backoff. */
	retry(): Promise<void>;
}

export const LOCAL_HOST_LABEL = localize('wispHost.thisMac', "this Mac");

const INITIAL_STATE: WispdState = { kind: 'disconnected', command: '', reason: 'notStarted', message: '' };

export class WispHostStatusService extends Disposable implements IWispHostStatusService {
	declare readonly _serviceBrand: undefined;

	private readonly _state = observableValue<WispdState>(this, INITIAL_STATE);
	readonly state: IObservable<WispdState> = this._state;

	private readonly _configuredHost = observableValue<string>(this, '');
	readonly configuredHost: IObservable<string> = this._configuredHost;

	private readonly _status = observableValue<IWispHostStatus>(this, undefined!);
	readonly status: IObservable<IWispHostStatus> = this._status;

	private command = '';
	private wasConnected = false;
	private lastProblem: WispHostProblem | undefined;
	/** The state arrives from the shared process; a slow first answer must not overwrite a newer event. */
	private receivedEvent = false;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@IConfigurationService private readonly configurationService: IConfigurationService,
	) {
		super();
		this._configuredHost.set(this.readHost(), undefined);
		this.update(INITIAL_STATE);

		this._register(configurationService.onDidChangeConfiguration(event => {
			if (event.affectsConfiguration(WISP_HOST_SETTING)) {
				this._configuredHost.set(this.readHost(), undefined);
				this.update(this._state.get());
			}
		}));
		this._register(wispdService.onDidChangeState(state => {
			this.receivedEvent = true;
			this.update(state);
		}));
		wispdService.getState().then(state => {
			if (!this.receivedEvent && !this._store.isDisposed) {
				this.update(state);
			}
		}, () => { /* The shared process is gone; the window is closing. */ });
	}

	retry(): Promise<void> {
		return this.wispdService.retry();
	}

	private readHost(): string {
		const value: unknown = this.configurationService.getValue(WISP_HOST_SETTING);
		return typeof value === 'string' ? value.trim() : '';
	}

	private update(state: WispdState): void {
		// A new command means another host (or another wispd on it): the old host's history no longer applies.
		if (state.command !== this.command) {
			this.command = state.command;
			this.wasConnected = false;
			this.lastProblem = undefined;
		}
		if (state.kind === 'connected') {
			this.wasConnected = true;
			this.lastProblem = undefined;
		} else if (state.kind === 'incompatible' || (state.kind === 'disconnected' && state.reason !== 'notStarted')) {
			this.lastProblem = state;
		}
		const host = this._configuredHost.get();
		this._state.set(state, undefined);
		this._status.set(describeHostStatus(state, {
			host: hostLabel(host, LOCAL_HOST_LABEL),
			remote: !isLocalHost(host),
			wasConnected: this.wasConnected,
			lastProblem: this.lastProblem,
		}), undefined);
	}
}
