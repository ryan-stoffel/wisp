/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import * as cp from 'child_process';
import type { Readable, Writable } from 'stream';
import { Emitter } from '../../../base/common/event.js';
import { Disposable, toDisposable } from '../../../base/common/lifecycle.js';
import { StreamSplitter } from '../../../base/node/nodeStreams.js';
import { ILogger } from '../../log/common/log.js';
import { IWispdTransport, IWispdTransportClose, IWispdTransportFactory } from '../common/wispdClient.js';
import { MAX_FRAME_BYTES } from '../common/wispProtocol.js';

const NEWLINE = 0x0a;
const STDERR_TAIL_BYTES = 4096;

/** `wispd attach`'s exit code when it never reached wispd (decision record 0010). */
export const ATTACH_EXIT_UNREACHABLE = 4;

/**
 * Splits a stream into lines with upstream's `StreamSplitter('\n')`. `StreamSplitter` buffers
 * without limit, so this counts the bytes of the line in progress first, and reports an overflow
 * instead of buffering a line longer than `maxFrameBytes`.
 */
export class WispdLineReader extends Disposable {

	private readonly _onLine = this._register(new Emitter<string>());
	readonly onLine = this._onLine.event;

	private readonly _onData = this._register(new Emitter<void>());
	readonly onData = this._onData.event;

	private readonly _onOverflow = this._register(new Emitter<number>());
	/** Fires once with the length reached, after which nothing else is read. */
	readonly onOverflow = this._onOverflow.event;

	private readonly splitter = new StreamSplitter('\n');
	private lineBytes = 0;
	private overflowed = false;

	constructor(stream: Readable, private readonly maxFrameBytes: number = MAX_FRAME_BYTES) {
		super();
		const onData = (chunk: Buffer) => this.onChunk(chunk);
		const onLine = (line: Buffer) => this.emitLine(line);
		stream.on('data', onData);
		this.splitter.on('data', onLine);
		this._register(toDisposable(() => {
			stream.off('data', onData);
			this.splitter.off('data', onLine);
			this.splitter.destroy();
		}));
	}

	private onChunk(chunk: Buffer): void {
		if (this.overflowed || this._store.isDisposed) {
			return;
		}
		this._onData.fire();

		let start = 0;
		let lineBytes = this.lineBytes;
		for (let index = chunk.indexOf(NEWLINE); index !== -1; index = chunk.indexOf(NEWLINE, start)) {
			if (lineBytes + index - start > this.maxFrameBytes) {
				this.overflow(lineBytes + index - start);
				return;
			}
			lineBytes = 0;
			start = index + 1;
		}
		lineBytes += chunk.length - start;
		if (lineBytes > this.maxFrameBytes) {
			this.overflow(lineBytes);
			return;
		}
		this.lineBytes = lineBytes;
		this.splitter.write(chunk);
	}

	private emitLine(line: Buffer): void {
		if (this.overflowed || this._store.isDisposed) {
			return;
		}
		let end = line.length;
		if (end > 0 && line[end - 1] === NEWLINE) {
			end--;
		}
		if (end > 0 && line[end - 1] === 0x0d) {
			end--;
		}
		this._onLine.fire(line.toString('utf8', 0, end));
	}

	private overflow(length: number): void {
		this.overflowed = true;
		this._onOverflow.fire(length);
	}
}

export interface IWispdProcessStreams {
	readonly stdin: Writable;
	readonly stdout: Readable;
	readonly stderr?: Readable;
}

/**
 * A transport over a child process's stdio, normally `wispd attach`. Closing it kills the
 * process; `attach` never stops wispd itself (decision record 0010).
 */
export class WispdProcessTransport extends Disposable implements IWispdTransport {

	private readonly _onDidClose = this._register(new Emitter<IWispdTransportClose>());
	readonly onDidClose = this._onDidClose.event;

	readonly onDidReceiveLine;
	readonly onDidReceiveData;

	private readonly reader: WispdLineReader;
	private stderrTail = '';
	private closed = false;
	private pendingClose: Omit<IWispdTransportClose, 'stderr'> | undefined;

	constructor(
		private readonly child: cp.ChildProcess & IWispdProcessStreams,
		private readonly command: string,
		private readonly logger: ILogger,
		maxFrameBytes: number = MAX_FRAME_BYTES,
	) {
		super();
		this.reader = this._register(new WispdLineReader(child.stdout, maxFrameBytes));
		this.onDidReceiveLine = this.reader.onLine;
		this.onDidReceiveData = this.reader.onData;

		this._register(this.reader.onOverflow(length => {
			this.pendingClose = { reason: 'frameTooLarge', message: `wispd sent a line of more than ${maxFrameBytes} bytes (${length} so far), so the connection was closed.` };
			this.kill();
		}));

		child.stdin.on('error', error => this.logger.trace(`wispd attach: stdin: ${error.message}`));
		child.stderr?.on('data', (chunk: Buffer) => {
			const text = chunk.toString('utf8');
			for (const line of text.split('\n')) {
				if (line.trim()) {
					this.logger.info(`${line.trimEnd()}`);
				}
			}
			this.stderrTail = (this.stderrTail + text).slice(-STDERR_TAIL_BYTES);
		});
		child.on('error', (error: NodeJS.ErrnoException) => {
			this.finish({
				reason: 'spawnFailed',
				message: error.code === 'ENOENT'
					? `${this.command} was not found. Install wisp, or set WISP_WISPD_PATH to a wispd binary.`
					: `Could not run ${this.command}: ${error.message}`,
			});
		});
		child.on('close', (code: number | null, signal: NodeJS.Signals | null) => {
			const exitCode = code ?? undefined;
			const unreachable = exitCode === ATTACH_EXIT_UNREACHABLE;
			this.finish(this.pendingClose ?? {
				reason: unreachable ? 'unreachable' : 'exited',
				message: unreachable
					? 'wispd attach could not reach or start wispd.'
					: `wispd attach exited${signal ? ` on ${signal}` : ` with code ${code}`}.`,
				exitCode,
			});
		});
	}

	/** The child's process id, for tests that stop it from outside. */
	get pid(): number | undefined {
		return this.child.pid;
	}

	send(line: string): void {
		if (!this.closed && this.child.stdin.writable) {
			this.child.stdin.write(line + '\n');
		}
	}

	override dispose(): void {
		this.kill();
		super.dispose();
	}

	private kill(): void {
		if (this.child.exitCode === null && this.child.signalCode === null) {
			this.child.kill('SIGTERM');
		}
	}

	private finish(close: Omit<IWispdTransportClose, 'stderr'>): void {
		if (this.closed) {
			return;
		}
		this.closed = true;
		const stderr = this.stderrTail.trim();
		this._onDidClose.fire({ ...close, stderr: stderr || undefined });
	}
}

export interface IWispdLaunch {
	/** The `wispd` executable. */
	readonly executable: string;
	/** Usually `['attach']`. */
	readonly args: readonly string[];
	readonly env?: NodeJS.ProcessEnv;
	readonly maxFrameBytes?: number;
}

export class WispdProcessTransportFactory implements IWispdTransportFactory {

	readonly command: string;

	constructor(private readonly launch: IWispdLaunch, private readonly logger: ILogger) {
		this.command = [launch.executable, ...launch.args].join(' ');
	}

	create(): IWispdTransport {
		const child = cp.spawn(this.launch.executable, [...this.launch.args], {
			stdio: ['pipe', 'pipe', 'pipe'],
			env: this.launch.env ?? process.env,
		});
		return new WispdProcessTransport(child, this.command, this.logger, this.launch.maxFrameBytes);
	}
}
