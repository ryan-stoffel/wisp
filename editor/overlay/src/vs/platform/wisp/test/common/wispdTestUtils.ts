/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Emitter } from '../../../../base/common/event.js';
import { IJsonRpcRequest } from '../../../../base/common/jsonRpcProtocol.js';
import { IDisposable } from '../../../../base/common/lifecycle.js';
import { IWispdTimers, IWispdTransport, IWispdTransportClose, IWispdTransportFactory } from '../../common/wispdClient.js';
import { EventsEventParams, InitializeResult, Project, WispEvent } from '../../common/wispProtocol.js';

interface ITask {
	readonly at: number;
	readonly order: number;
	readonly callback: () => void;
	cancelled: boolean;
}

/** Timers that run only when a test advances them. */
export class FakeTimers implements IWispdTimers {
	private time = 0;
	private order = 0;
	private tasks: ITask[] = [];

	now(): number {
		return this.time;
	}

	setTimeout(callback: () => void, ms: number): IDisposable {
		const task: ITask = { at: this.time + ms, order: this.order++, callback, cancelled: false };
		this.tasks.push(task);
		return { dispose: () => { task.cancelled = true; } };
	}

	/** Runs every timer due in the next `ms`, in order, settling promises after each. */
	async advance(ms: number): Promise<void> {
		const end = this.time + ms;
		for (; ;) {
			this.tasks = this.tasks.filter(task => !task.cancelled);
			const next = this.tasks.filter(task => task.at <= end).sort((a, b) => a.at - b.at || a.order - b.order)[0];
			if (!next) {
				break;
			}
			this.tasks.splice(this.tasks.indexOf(next), 1);
			this.time = next.at;
			next.callback();
			await settle();
		}
		this.time = end;
		await settle();
	}
}

/** Lets pending promise callbacks run. */
export async function settle(): Promise<void> {
	for (let i = 0; i < 20; i++) {
		await Promise.resolve();
	}
}

export class FakeTransport implements IWispdTransport {
	private readonly _onDidReceiveLine = new Emitter<string>();
	readonly onDidReceiveLine = this._onDidReceiveLine.event;
	private readonly _onDidReceiveData = new Emitter<void>();
	readonly onDidReceiveData = this._onDidReceiveData.event;
	private readonly _onDidClose = new Emitter<IWispdTransportClose>();
	readonly onDidClose = this._onDidClose.event;

	readonly sent: Array<{ id?: number | string; method?: string; params?: unknown }> = [];
	disposed = false;

	constructor(private readonly onSend: (transport: FakeTransport, message: IJsonRpcRequest) => void) { }

	send(line: string): void {
		const message = JSON.parse(line);
		this.sent.push(message);
		this.onSend(this, message);
	}

	/** wispd writes one message. */
	receive(message: unknown): void {
		this.receiveData();
		this._onDidReceiveLine.fire(JSON.stringify(message));
	}

	/** wispd writes a line that may not be JSON. */
	receiveLine(line: string): void {
		this.receiveData();
		this._onDidReceiveLine.fire(line);
	}

	/** Bytes arrive without completing a line. */
	receiveData(): void {
		this._onDidReceiveData.fire();
	}

	close(close: IWispdTransportClose = { reason: 'exited', message: 'wispd attach exited with code 0.', exitCode: 0 }): void {
		this._onDidClose.fire(close);
	}

	requests(method: string): Array<{ id?: number | string; method?: string; params?: unknown }> {
		return this.sent.filter(message => message.method === method);
	}

	dispose(): void {
		this.disposed = true;
		this._onDidReceiveLine.dispose();
		this._onDidReceiveData.dispose();
		this._onDidClose.dispose();
	}
}

/**
 * A stand-in for wispd behind `attach`. It answers on a microtask, as wispd would after a read,
 * and writes a subscription's replayed events right after its answer, in the same turn.
 */
export class FakeWispd implements IWispdTransportFactory {
	readonly command = 'wispd attach';
	readonly transports: FakeTransport[] = [];

	logId = 'log-1';
	capabilities: InitializeResult['capabilities'] = {};
	/** When false, requests get no answer. */
	answering = true;
	/** When set, `initialize` fails with `incompatibleProtocol` and this range. */
	incompatible: { min: number; max: number } | undefined;
	/** When true, `events/subscribe` fails with `resyncRequired`. */
	resyncRequired = false;
	/** When set, `create` throws it, as a failed spawn would. */
	spawnError: Error | undefined;

	readonly projects: Project[] = [];
	readonly log: Array<{ seq: number; event: WispEvent; project?: string }> = [];
	private nextSubscription = 1;
	private readonly live = new Map<string, { transport: FakeTransport; project?: string }>();

	get current(): FakeTransport {
		return this.transports[this.transports.length - 1];
	}

	create(): IWispdTransport {
		if (this.spawnError) {
			throw this.spawnError;
		}
		const transport = new FakeTransport((t, message) => queueMicrotask(() => this.answer(t, message)));
		this.transports.push(transport);
		return transport;
	}

	/** Appends an event to the log and sends it to every live subscription. */
	emit(event: WispEvent, project?: string): void {
		const seq = this.log.length + 1;
		this.log.push({ seq, event, project });
		for (const [subscription, target] of this.live) {
			if (!target.transport.disposed && target.project === project) {
				target.transport.receive({ jsonrpc: '2.0', method: 'events/event', params: this.eventParams(subscription, seq) });
			}
		}
	}

	/** Starts the log over, as a restarted M1 wispd does. */
	restart(logId: string): void {
		this.logId = logId;
		this.log.length = 0;
		this.live.clear();
	}

	private eventParams(subscription: string, seq: number): EventsEventParams {
		const entry = this.log[seq - 1];
		return { subscription, seq, time: '2026-09-24T00:00:00Z', ...(entry.project ? { project: entry.project } : {}), event: entry.event };
	}

	private answer(transport: FakeTransport, message: IJsonRpcRequest): void {
		if (message.id === undefined || !this.answering || transport.disposed) {
			return;
		}
		const reply = (result: unknown) => transport.receive({ jsonrpc: '2.0', id: message.id, result });
		const fail = (kind: string, detail?: unknown) => transport.receive({ jsonrpc: '2.0', id: message.id, error: { code: -32000, message: kind, data: { kind, ...(detail ? { detail } : {}) } } });
		const params = message.params as Record<string, unknown>;

		switch (message.method) {
			case 'initialize':
				if (this.incompatible) {
					return fail('incompatibleProtocol', { requested: params.protocol, supported: this.incompatible, wispd: '9.0.0' });
				}
				return reply({ protocol: 1, wispd: '0.1.0', logId: this.logId, capabilities: this.capabilities, maxFrameBytes: 8388608 });
			case 'host/health':
				return reply({ uptimeSeconds: 5, store: 'ok', runningAgents: 0 });
			case 'project/list':
				return reply({ projects: this.projects, seq: this.log.length });
			case 'project/create': {
				const existing = this.projects.find(project => project.id === params.id);
				if (existing) {
					return existing.name === params.name && existing.repoPath === params.repoPath ? reply({ project: existing }) : fail('idConflict');
				}
				const project: Project = { id: params.id as string, name: params.name as string, repoPath: params.repoPath as string, createdAt: '2026-09-24T00:00:00Z', updatedAt: '2026-09-24T00:00:00Z' };
				this.projects.push(project);
				reply({ project });
				this.emit({ kind: 'project.created', project });
				return;
			}
			case 'events/subscribe': {
				if (this.resyncRequired) {
					return fail('resyncRequired');
				}
				const subscription = `sub-${this.nextSubscription++}`;
				const project = params.project as string | undefined;
				this.live.set(subscription, { transport, project });
				reply({ subscription });
				for (const entry of this.log) {
					if (entry.seq > (params.after as number) && entry.project === project) {
						transport.receive({ jsonrpc: '2.0', method: 'events/event', params: this.eventParams(subscription, entry.seq) });
					}
				}
				return;
			}
			case 'events/unsubscribe':
				this.live.delete(params.subscription as string);
				return reply({});
			default:
				return transport.receive({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Method not found' } });
		}
	}
}
