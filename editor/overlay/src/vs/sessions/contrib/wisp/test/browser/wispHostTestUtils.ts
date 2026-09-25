/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Emitter, Event } from '../../../../../base/common/event.js';
import { IWispdService, WispdDisconnectReason, WispdState, WispdSubscriptionMessage } from '../../../../../platform/wisp/common/wispd.js';

export const LOCAL_COMMAND = '/Applications/Wisp.app/Contents/Resources/app/bin/wispd attach';
export const SSH_COMMAND = 'ssh -T -o BatchMode=yes -- mac-mini wispd attach';

export function connected(command = LOCAL_COMMAND): WispdState {
	return { kind: 'connected', command, wispd: '0.1.0', protocol: 1, logId: 'log-1', capabilities: {}, maxFrameBytes: 8 * 1024 * 1024 };
}

export function connecting(attempt: number, command = LOCAL_COMMAND): WispdState {
	return { kind: 'connecting', command, attempt };
}

export function disconnected(reason: WispdDisconnectReason, command = LOCAL_COMMAND, message = `${reason} happened.`): Extract<WispdState, { kind: 'disconnected' }> {
	return { kind: 'disconnected', command, reason, message, retryAt: 10_000 };
}

export function incompatible(update: 'editor' | 'wispd', command = SSH_COMMAND): Extract<WispdState, { kind: 'incompatible' }> {
	return { kind: 'incompatible', command, update, wispd: '0.9.0', requested: { min: 1, max: 1 }, supported: { min: 2, max: 3 } };
}

/** An `IWispdService` whose state the test sets. */
export class TestWispdService implements IWispdService {
	declare readonly _serviceBrand: undefined;

	private readonly emitter = new Emitter<WispdState>();
	readonly onDidChangeState = this.emitter.event;
	retries = 0;

	constructor(private state: WispdState = { kind: 'disconnected', command: LOCAL_COMMAND, reason: 'notStarted', message: 'Not connected yet.' }) { }

	/** Answers with the state as it is now, like the shared process, but a turn later. */
	getState(): Promise<WispdState> {
		const state = this.state;
		return Promise.resolve().then(() => state);
	}

	async retry(): Promise<void> {
		this.retries++;
	}

	request(): Promise<never> {
		return Promise.reject(new Error('not connected'));
	}

	subscribe(): Event<WispdSubscriptionMessage> {
		return Event.None;
	}

	setState(state: WispdState): void {
		this.state = state;
		this.emitter.fire(state);
	}

	dispose(): void {
		this.emitter.dispose();
	}
}
