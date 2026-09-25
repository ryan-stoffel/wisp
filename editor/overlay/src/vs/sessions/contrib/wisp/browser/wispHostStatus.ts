/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { localize } from '../../../../nls.js';
import { describeIncompatible, WispdDisconnectReason, WispdState } from '../../../../platform/wisp/common/wispd.js';

/**
 * The mark next to the host's name. Each has its own shape, so state never rests on color alone:
 * a filled dot, a dashed ring, a hollow dot, and a diamond.
 */
export type WispHostMark = 'connected' | 'connecting' | 'idle' | 'error';

/** What the no-host view offers for a state, in order. */
export type WispHostAction = 'retry' | 'switchHost' | 'openSettings' | 'showLog';

/** A state that says why the host is down, kept while the client retries so the copy doesn't flicker. */
export type WispHostProblem = Extract<WispdState, { kind: 'disconnected' | 'incompatible' }>;

export interface IWispHostStatus {
	readonly kind: 'connected' | 'connecting' | 'notConnected' | 'error';
	/** "this Mac", or the ssh destination. */
	readonly host: string;
	readonly mark: WispHostMark;
	/** The state in words, lowercase, for the chip and its accessible name: "connected", "offline". */
	readonly state: string;
	/** Whether the chip shows `state` next to the host. A connected chip shows the host alone, as in the mockup. */
	readonly showState: boolean;
	/** The chip's accessible name: "Host: mac-mini, connected". */
	readonly ariaLabel: string;
	/** The no-host view's heading, and the Quick Pick's detail for the current host while it is down. */
	readonly heading: string;
	readonly body: string;
	readonly placeholder: string;
	readonly actions: readonly WispHostAction[];
	/** True while the client tries again after a problem: the view keeps the problem's copy. */
	readonly retrying: boolean;
	/** The connected wispd's version, for the Quick Pick. */
	readonly wispd?: string;
}

export interface IWispHostContext {
	/** "this Mac", or the ssh destination, from `hostLabel`. */
	readonly host: string;
	/** Whether `wisp.host` names an ssh destination rather than this Mac. */
	readonly remote: boolean;
	/** Whether this window has seen the host connected since the host last changed. */
	readonly wasConnected: boolean;
	/** The last problem since the host last connected, if any. */
	readonly lastProblem?: WispHostProblem;
}

const NO_HOST_BODY = localize('wispHost.noHostBody', "The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.");
const RECONNECT_PLACEHOLDER = localize('wispHost.reconnectPlaceholder', "Reconnect to send messages");
const NOT_CONNECTED_PLACEHOLDER = localize('wispHost.notConnectedPlaceholder', "Not connected to a host");
const PROBLEM_ACTIONS: readonly WispHostAction[] = ['retry', 'switchHost', 'showLog'];

/** Reasons that mean an established connection went away, when the host had been connected. */
const LOST_REASONS: ReadonlySet<WispdDisconnectReason> = new Set(['noRoute', 'exited', 'timedOut', 'protocolError', 'frameTooLarge']);

/** Maps the connection state to everything the chip, the no-host view, and the host menu show. */
export function describeHostStatus(state: WispdState, context: IWispHostContext): IWispHostStatus {
	const { host } = context;
	switch (state.kind) {
		case 'connected':
			return {
				kind: 'connected',
				host,
				mark: 'connected',
				state: localize('wispHost.connected', "connected"),
				showState: false,
				ariaLabel: ariaLabel(host, localize('wispHost.connected', "connected")),
				heading: localize('wispHost.connectedHeading', "Connected to {0}", host),
				body: '',
				placeholder: '',
				actions: [],
				retrying: false,
				wispd: state.wispd,
			};
		case 'connecting':
			if (context.lastProblem) {
				return { ...describeProblem(context.lastProblem, context), retrying: true };
			}
			return notConnected(context, localize('wispHost.connecting', "connecting"), 'connecting', 'connecting');
		case 'disconnected':
			if (state.reason === 'notStarted') {
				return notConnected(context, localize('wispHost.notConnected', "not connected"), 'notConnected', 'idle');
			}
			return describeProblem(state, context);
		case 'incompatible':
			return describeProblem(state, context);
	}
}

function notConnected(context: IWispHostContext, state: string, kind: 'notConnected' | 'connecting', mark: WispHostMark): IWispHostStatus {
	return {
		kind,
		host: context.host,
		mark,
		state,
		showState: true,
		ariaLabel: ariaLabel(context.host, state),
		heading: localize('wispHost.noHostHeading', "No host connected"),
		body: NO_HOST_BODY,
		placeholder: NOT_CONNECTED_PLACEHOLDER,
		actions: [],
		retrying: false,
	};
}

function describeProblem(problem: WispHostProblem, context: IWispHostContext): IWispHostStatus {
	const { host } = context;
	const copy = problemCopy(problem, context);
	return {
		kind: 'error',
		host,
		mark: 'error',
		state: copy.state,
		showState: true,
		ariaLabel: ariaLabel(host, copy.state),
		heading: copy.heading,
		body: copy.body,
		placeholder: RECONNECT_PLACEHOLDER,
		actions: copy.actions ?? PROBLEM_ACTIONS,
		retrying: false,
	};
}

interface IProblemCopy {
	readonly state: string;
	readonly heading: string;
	readonly body: string;
	readonly actions?: readonly WispHostAction[];
}

function problemCopy(problem: WispHostProblem, context: IWispHostContext): IProblemCopy {
	const { host, remote } = context;
	const retries = localize('wispHost.retries', "wisp retries every 10 seconds.");
	if (problem.kind === 'incompatible') {
		return {
			state: localize('wispHost.stateIncompatible', "needs an update"),
			heading: problem.update === 'wispd'
				? localize('wispHost.incompatibleWispd', "wispd on {0} needs an update", host)
				: localize('wispHost.incompatibleEditor', "Wisp needs an update to use {0}", host),
			body: describeIncompatible(problem, host),
		};
	}
	if (context.wasConnected && LOST_REASONS.has(problem.reason)) {
		return {
			state: localize('wispHost.stateOffline', "offline"),
			heading: localize('wispHost.lostHeading', "Lost connection to {0}.", host),
			body: localize('wispHost.lostBody', "wisp retries every 10 seconds. Agents already running on the host keep going."),
		};
	}
	switch (problem.reason) {
		case 'noRoute':
			return {
				state: localize('wispHost.stateUnreachable', "unreachable"),
				heading: localize('wispHost.noRouteHeading', "Can't reach {0}.", host),
				body: localize('wispHost.noRouteBody', "ssh couldn't connect to {0}. Check that it's on and reachable from this Mac. {1}", host, retries),
			};
		case 'authFailed':
			return {
				state: localize('wispHost.stateAuth', "sign-in failed"),
				heading: localize('wispHost.authHeading', "ssh couldn't sign in to {0}.", host),
				body: localize('wispHost.authBody', "wisp uses your ssh config, keys, and agent, and never asks for a password. Run `ssh {0}` in a terminal to unlock your key or fix the sign-in, then retry.", host),
			};
		case 'hostKeyUnknown':
			return {
				state: localize('wispHost.stateKeyUnknown', "host key unknown"),
				heading: localize('wispHost.keyUnknownHeading', "ssh doesn't know {0}'s host key yet.", host),
				body: localize('wispHost.keyUnknownBody', "wisp can't answer ssh's question about a new host. Run `ssh {0}` in a terminal and check the fingerprint against one from the host itself, then retry.", host),
			};
		case 'hostKeyChanged':
			return {
				state: localize('wispHost.stateKeyChanged', "host key changed"),
				heading: localize('wispHost.keyChangedHeading', "{0}'s host key has changed.", host),
				body: localize('wispHost.keyChangedBody', "The key {0} sent doesn't match the one ssh saved for it. This can mean someone is intercepting the connection, so wisp won't connect. If {0} was reinstalled, confirm its new fingerprint with whoever runs it before you remove the old key from known_hosts.", host),
			};
		case 'wispdNotFound':
			return {
				state: localize('wispHost.stateNotInstalled', "wispd not found"),
				heading: localize('wispHost.notFoundHeading', "wispd isn't installed on {0}.", host),
				body: localize('wispHost.notFoundBody', "wisp looked on the host's PATH, in /opt/homebrew/bin, and in /usr/local/bin. Install wispd there, or set Wisp: Remote Wispd Path, then retry."),
				actions: ['retry', 'openSettings', 'showLog'],
			};
		case 'unreachable':
			return {
				state: localize('wispHost.stateNotRunning', "wispd not running"),
				heading: localize('wispHost.notRunningHeading', "wispd isn't running on {0}.", host),
				body: remote
					? localize('wispHost.notRunningRemoteBody', "Start it on the host, then retry.")
					: localize('wispHost.notRunningLocalBody', "wisp couldn't start it. The wispd log says why."),
			};
		case 'spawnFailed':
			return remote
				? {
					state: localize('wispHost.stateNoSsh', "ssh failed"),
					heading: localize('wispHost.noSshHeading', "wisp couldn't run ssh."),
					body: problem.message,
				}
				: {
					state: localize('wispHost.stateNoWispd', "can't start wispd"),
					heading: localize('wispHost.noWispdHeading', "wisp couldn't start wispd."),
					body: localize('wispHost.noWispdBody', "The wispd that comes with Wisp is missing or can't run. Reinstalling Wisp puts it back. {0}", problem.message),
				};
		case 'invalidHost':
			return {
				state: localize('wispHost.stateInvalid', "invalid host"),
				heading: localize('wispHost.invalidHeading', "The Wisp: Host setting isn't a usable host."),
				body: problem.message,
				actions: ['switchHost', 'openSettings', 'showLog'],
			};
		case 'protocolError':
			return {
				state: localize('wispHost.stateOffline', "offline"),
				heading: localize('wispHost.protocolHeading', "wispd on {0} sent something wisp can't read.", host),
				body: remote
					? localize('wispHost.protocolRemoteBody', "A shell startup file on the host may be printing text. {0}", problem.message)
					: problem.message,
			};
		default:
			return {
				state: localize('wispHost.stateOffline', "offline"),
				heading: localize('wispHost.cantConnectHeading', "Can't connect to {0}.", host),
				body: `${problem.message} ${retries}`,
			};
	}
}

function ariaLabel(host: string, state: string): string {
	return localize('wispHost.ariaLabel', "Host: {0}, {1}", host, state);
}
