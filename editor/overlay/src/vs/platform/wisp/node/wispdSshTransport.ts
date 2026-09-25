/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import * as cp from 'child_process';
import { homedir } from 'os';
import { Emitter, Event } from '../../../base/common/event.js';
import { Disposable, DisposableStore } from '../../../base/common/lifecycle.js';
import { sanitizeProcessEnvironment } from '../../../base/common/processes.js';
import { ILogger } from '../../log/common/log.js';
import { IWispdTransport, IWispdTransportClose, IWispdTransportFactory } from '../common/wispdClient.js';
import { ATTACH_EXIT_UNREACHABLE, IWispdProcessStreams, WispdCloseClassifier, WispdProcessTransport } from './wispdTransport.js';

/** ssh's own exit code for a connection-level failure: no route, a timeout, auth, or a host key (`ssh(1)`). */
export const SSH_CONNECTION_FAILURE_EXIT_CODE = 255;

/** The remote shell's exit code when the command it was asked to run isn't on its `PATH`. */
export const REMOTE_COMMAND_NOT_FOUND_EXIT_CODE = 127;

/**
 * `wispd` on a host's `PATH`, then the paths Homebrew uses on Apple silicon and on Intel
 * (decision record 0007; see daemon/README.md for why a non-interactive SSH `PATH` misses these).
 */
export const DEFAULT_REMOTE_WISPD_CANDIDATES: readonly string[] = ['wispd', '/opt/homebrew/bin/wispd', '/usr/local/bin/wispd'];

const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/;

/**
 * Rejects an ssh destination that ssh could misread as an option, or that could carry stray bytes
 * into the argument vector. Returns why it's invalid, or `undefined` if it's fine to use.
 */
export function validateSshDestination(destination: string): string | undefined {
	if (destination.length === 0) {
		return 'it is empty';
	}
	if (destination.startsWith('-')) {
		return 'it starts with "-", which ssh would read as an option';
	}
	if (/\s/.test(destination)) {
		return 'it contains whitespace';
	}
	if (CONTROL_CHARACTER.test(destination)) {
		return 'it contains a control character';
	}
	return undefined;
}

/** The exact argument vector from decision record 0007, run with no shell. */
export function buildSshArgs(destination: string, remoteWispd: string): string[] {
	return ['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ControlPath=none', '--', destination, remoteWispd, 'attach'];
}

/**
 * Maps ssh's exit codes and stderr to a close reason (decision record 0007). Exit 4 still means
 * `wispd attach` ran on the host and could not reach or start wispd there. Exit 127 means the
 * remote shell never found the command. Exit 255 is ssh's own catch-all for a connection that
 * never got to run anything, which stderr is the only way to tell apart.
 */
export const classifySshExit: WispdCloseClassifier = (exitCode, signal, stderrTail) => {
	if (exitCode === ATTACH_EXIT_UNREACHABLE) {
		return { reason: 'unreachable', message: 'wispd attach could not reach or start wispd on the host.', exitCode };
	}
	if (exitCode === REMOTE_COMMAND_NOT_FOUND_EXIT_CODE) {
		return {
			reason: 'wispdNotFound',
			message: "The host's shell could not find wispd. Install it there with Homebrew, add it to PATH in ~/.zshenv, or set wisp.remoteWispdPath to its absolute path.",
			exitCode,
		};
	}
	if (exitCode === SSH_CONNECTION_FAILURE_EXIT_CODE) {
		return classifySshConnectionFailure(stderrTail, exitCode);
	}
	return {
		reason: 'exited',
		message: `ssh exited${signal ? ` on ${signal}` : ` with code ${exitCode}`}.`,
		exitCode,
	};
};

function classifySshConnectionFailure(stderrTail: string, exitCode: number): Omit<IWispdTransportClose, 'stderr'> {
	const text = stderrTail.toLowerCase();
	if (text.includes('permission denied') || text.includes('authentication failed') || text.includes('too many authentication failures')) {
		return {
			reason: 'authFailed',
			message: 'ssh could not authenticate. BatchMode turns any prompt into this error, including one for interactive two-factor login, which is not supported. Run "ssh <destination>" in a terminal once to unlock your key.',
			exitCode,
		};
	}
	if (text.includes('host key verification failed') || text.includes('remote host identification has changed')) {
		return {
			reason: 'hostKeyUnknown',
			message: 'ssh doesn\'t recognize the host\'s key yet, and BatchMode can\'t prompt to accept it. Run "ssh <destination>" in a terminal once to accept it.',
			exitCode,
		};
	}
	if (text.includes('could not resolve hostname') || text.includes('name or service not known') || text.includes('no route to host')
		|| text.includes('network is unreachable') || text.includes('connection refused') || text.includes('operation timed out') || text.includes('connection timed out')) {
		return { reason: 'noRoute', message: 'ssh could not reach the host: no route, or the attempt timed out.', exitCode };
	}
	return { reason: 'exited', message: `ssh exited with code ${exitCode}.`, exitCode };
}

/**
 * A transport that spawns ssh to reach `wispd attach` on a host (decision record 0007). It tries
 * `candidates` for the remote `wispd` in order, respawning with the next one whenever the shell
 * answers 127 (command not found), so the reconnect state machine in `WispdClient` still sees one
 * connection attempt per candidate list, not one per candidate.
 */
export class WispdSshTransport extends Disposable implements IWispdTransport {

	private readonly _onDidReceiveLine = this._register(new Emitter<string>());
	readonly onDidReceiveLine = this._onDidReceiveLine.event;

	private readonly _onDidReceiveData = this._register(new Emitter<void>());
	readonly onDidReceiveData = this._onDidReceiveData.event;

	private readonly _onDidClose = this._register(new Emitter<IWispdTransportClose>());
	readonly onDidClose = this._onDidClose.event;

	private readonly current = this._register(new DisposableStore());
	private transport: WispdProcessTransport | undefined;
	private index = 0;
	private done = false;
	/**
	 * Lines sent to a candidate that turned out to be missing never reached anything: the shell
	 * exited before reading stdin. Replaying them to the next candidate resends `initialize`,
	 * which `WispdClient` otherwise believes it already sent. Cleared once a candidate answers
	 * with anything, since retries only happen before that.
	 */
	private readonly sent: string[] = [];

	constructor(
		private readonly destination: string,
		private readonly candidates: readonly string[],
		private readonly spawn: (args: readonly string[]) => cp.ChildProcess & IWispdProcessStreams,
		private readonly command: string,
		private readonly logger: ILogger,
		private readonly maxFrameBytes: number | undefined,
	) {
		super();
		this.spawnNext();
	}

	/** The current attempt's process id, for tests that stop it from outside. */
	get pid(): number | undefined {
		return this.transport?.pid;
	}

	send(line: string): void {
		this.sent.push(line);
		this.transport?.send(line);
	}

	private spawnNext(): void {
		this.current.clear();
		const remoteWispd = this.candidates[this.index];
		const child = this.spawn(buildSshArgs(this.destination, remoteWispd));
		const transport = this.current.add(new WispdProcessTransport(child, this.command, this.logger, this.maxFrameBytes, classifySshExit));
		this.transport = transport;
		this.current.add(transport.onDidReceiveLine(line => this._onDidReceiveLine.fire(line)));
		this.current.add(transport.onDidReceiveData(() => {
			// A candidate that answers anything is the one that's staying; nothing after this
			// point should be replayed to a future respawn.
			this.sent.length = 0;
			this._onDidReceiveData.fire();
		}));
		this.current.add(transport.onDidClose(close => this.onTransportClose(close)));
		for (const line of this.sent) {
			transport.send(line);
		}
	}

	private onTransportClose(close: IWispdTransportClose): void {
		if (this.done || this._store.isDisposed) {
			return;
		}
		if (close.reason === 'wispdNotFound' && this.index + 1 < this.candidates.length) {
			this.index++;
			this.spawnNext();
			return;
		}
		this.done = true;
		this._onDidClose.fire(close);
	}
}

export interface IWispdSshLaunch {
	readonly destination: string;
	/** Tried in order; the first that doesn't answer 127 wins. */
	readonly remoteWispdCandidates: readonly string[];
	/** Defaults to `ssh` on `PATH`. */
	readonly sshExecutable?: string;
	readonly env?: NodeJS.ProcessEnv;
	readonly cwd?: string;
	readonly maxFrameBytes?: number;
}

export class WispdSshTransportFactory implements IWispdTransportFactory {

	readonly command: string;

	constructor(private readonly launch: IWispdSshLaunch, private readonly logger: ILogger) {
		this.command = `${launch.sshExecutable ?? 'ssh'} -T -o BatchMode=yes -o ConnectTimeout=10 -o ControlPath=none -- ${launch.destination} wispd attach`;
	}

	create(): IWispdTransport {
		const sshExecutable = this.launch.sshExecutable ?? 'ssh';
		const spawn = (args: readonly string[]) => {
			// A serve that attach starts on the host outlives this ssh session, so it should not
			// inherit the editor's own VSCODE_* and ELECTRON_* variables (decision record 0010).
			const env = { ...(this.launch.env ?? process.env) };
			sanitizeProcessEnvironment(env);
			return cp.spawn(sshExecutable, [...args], {
				stdio: ['pipe', 'pipe', 'pipe'],
				env,
				cwd: this.launch.cwd ?? homedir(),
			}) as cp.ChildProcess & IWispdProcessStreams;
		};
		return new WispdSshTransport(this.launch.destination, this.launch.remoteWispdCandidates, spawn, this.command, this.logger, this.launch.maxFrameBytes);
	}
}

/** A transport that closes on its own, for a `wisp.host` that isn't usable (decision record 0007). */
export class WispdInvalidHostTransportFactory implements IWispdTransportFactory {

	readonly command: string;

	constructor(private readonly host: string, private readonly reason: string) {
		this.command = `ssh -- ${host} wispd attach`;
	}

	create(): IWispdTransport {
		return new ImmediateCloseTransport({
			reason: 'invalidHost',
			message: `wisp.host ("${this.host}") is not a valid ssh destination: ${this.reason}.`,
		});
	}
}

class ImmediateCloseTransport extends Disposable implements IWispdTransport {
	readonly onDidReceiveLine = Event.None;
	readonly onDidReceiveData = Event.None;

	private readonly _onDidClose = this._register(new Emitter<IWispdTransportClose>());
	readonly onDidClose = this._onDidClose.event;

	constructor(close: IWispdTransportClose) {
		super();
		// Fires after `create()` returns, once the caller has had a chance to listen.
		queueMicrotask(() => {
			if (!this._store.isDisposed) {
				this._onDidClose.fire(close);
			}
		});
	}

	send(): void {
		// Nothing to send: the connection never opens.
	}
}
