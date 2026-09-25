/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { existsSync, promises as fs } from 'fs';
import { tmpdir } from 'os';
import { join } from '../../../../base/common/path.js';
import { Event } from '../../../../base/common/event.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { NullLogger } from '../../../log/common/log.js';
import { IWispdSubscribeOptions, WispdState, WispdSubscriptionMessage } from '../../common/wispd.js';
import { IWispdClientOptions, IWispdTransport, IWispdTransportFactory, WispdClient } from '../../common/wispdClient.js';
import { generateUuidV7 } from '../../common/uuidv7.js';
import { WispdProcessTransport, WispdProcessTransportFactory } from '../../node/wispdTransport.js';

/**
 * Runs the client against a real wispd: set WISP_TEST_WISPD to a binary from `cargo build -p
 * wispd`. scripts/editor/test-wisp does, and CI runs it. Each test gets its own data folder.
 */
const wispdPath = process.env.WISP_TEST_WISPD;

class RecordingFactory implements IWispdTransportFactory {
	readonly transports: WispdProcessTransport[] = [];
	private readonly inner: WispdProcessTransportFactory;

	constructor(dataDir: string) {
		this.inner = new WispdProcessTransportFactory({ executable: wispdPath!, args: ['attach'], env: { ...process.env, WISPD_DATA_DIR: dataDir } }, new NullLogger());
	}

	get command(): string {
		return this.inner.command;
	}

	create(): IWispdTransport {
		const transport = this.inner.create() as WispdProcessTransport;
		this.transports.push(transport);
		return transport;
	}
}

function within<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
	let handle: ReturnType<typeof setTimeout>;
	const timeout = new Promise<never>((_, reject) => handle = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms));
	return Promise.race([promise, timeout]).finally(() => clearTimeout(handle));
}

function whenState<K extends WispdState['kind']>(client: WispdClient, kind: K): Promise<Extract<WispdState, { kind: K }>> {
	if (client.state.kind === kind) {
		return Promise.resolve(client.state as Extract<WispdState, { kind: K }>);
	}
	return within(Event.toPromise(Event.filter(client.onDidChangeState, state => state.kind === kind)) as unknown as Promise<Extract<WispdState, { kind: K }>>, 30_000, `state ${kind}`);
}

suite('wispd (integration)', function () {

	this.timeout(60_000);

	let dataDir: string;
	let repoPath: string;
	let store: DisposableStore;

	setup(async function () {
		if (!wispdPath) {
			this.skip();
		}
		assert.ok(existsSync(wispdPath), `WISP_TEST_WISPD is ${wispdPath}, which does not exist`);
		dataDir = await fs.mkdtemp(join(tmpdir(), 'wispd-'));
		// A new project needs a repository; this is enough of one for wispd's check.
		repoPath = join(dataDir, 'repo');
		await fs.mkdir(join(repoPath, '.git'), { recursive: true });
		await fs.writeFile(join(repoPath, '.git', 'HEAD'), 'ref: refs/heads/main\n');
		store = new DisposableStore();
	});

	teardown(async () => {
		store?.dispose();
		if (dataDir) {
			await stopDaemon(dataDir);
			await fs.rm(dataDir, { recursive: true, force: true });
		}
	});

	function client(options: Partial<IWispdClientOptions> = {}): { client: WispdClient; factory: RecordingFactory } {
		const factory = new RecordingFactory(dataDir);
		const client = store.add(new WispdClient(factory, { client: { name: 'wisp-test', version: '0.0.0' }, ...options }, new NullLogger()));
		return { client, factory };
	}

	function collect(client: WispdClient, options: IWispdSubscribeOptions): { messages: WispdSubscriptionMessage[]; when: (predicate: (messages: WispdSubscriptionMessage[]) => boolean, what: string) => Promise<void> } {
		const messages: WispdSubscriptionMessage[] = [];
		const waiters: Array<() => void> = [];
		store.add(client.subscribe(options, message => {
			messages.push(message);
			waiters.splice(0).forEach(wake => wake());
		}));
		const when = async (predicate: (messages: WispdSubscriptionMessage[]) => boolean, what: string) => {
			while (!predicate(messages)) {
				await within(new Promise<void>(resolve => waiters.push(resolve)), 30_000, what);
			}
		};
		return { messages, when };
	}

	const createdIds = (messages: WispdSubscriptionMessage[]) => messages.flatMap(message => message.type === 'event' && message.event.event.kind === 'project.created' ? [message.event.event.project.id] : []);

	test('handshake, list, create, and replay after the attach process dies', async () => {
		// A longer first backoff leaves time to create a project while this client is away.
		const { client: first, factory } = client({ minBackoffMs: 3_000 });
		first.start();
		const connected = await whenState(first, 'connected');
		assert.strictEqual(connected.protocol, 1);
		assert.strictEqual(connected.maxFrameBytes, 8 * 1024 * 1024);
		assert.match(connected.logId, /\S/);

		const listed = await first.request('project/list', {});
		assert.deepStrictEqual(listed.projects, []);

		const events = collect(first, { after: listed.seq, logId: connected.logId });
		const one = generateUuidV7();
		const created = await first.request('project/create', { id: one, name: 'one', repoPath });
		assert.strictEqual(created.project.id, one);
		await events.when(messages => createdIds(messages).includes(one), 'project one');

		process.kill(factory.transports[0].pid!, 'SIGKILL');
		await whenState(first, 'disconnected');

		const { client: second } = client();
		const two = generateUuidV7();
		await second.request('project/create', { id: two, name: 'two', repoPath });
		assert.strictEqual(first.state.kind, 'disconnected', 'the first client was still away when project two was created');

		await whenState(first, 'connected');
		await events.when(messages => createdIds(messages).includes(two), 'project two, replayed');
		assert.deepStrictEqual(createdIds(events.messages), [one, two]);
		const seqs = events.messages.map(message => message.type === 'event' ? message.event.seq : -1);
		assert.deepStrictEqual(seqs, [listed.seq + 1, listed.seq + 2]);
		assert.strictEqual(factory.transports.length, 2);
	});

	test('a restarted wispd is reached again, and its new log means resync', async () => {
		const { client: first } = client();
		first.start();
		const before = await whenState(first, 'connected');
		const one = generateUuidV7();
		await first.request('project/create', { id: one, name: 'one', repoPath });
		const events = collect(first, { after: 0, logId: before.logId });
		await events.when(messages => messages.length > 0, 'the replayed project');

		await stopDaemon(dataDir);
		await whenState(first, 'disconnected');
		const after = await whenState(first, 'connected');
		assert.notStrictEqual(after.logId, before.logId, 'M1 keeps its log in memory, so a new wispd has a new log');

		await events.when(messages => messages.some(message => message.type === 'resync'), 'resync');
		assert.deepStrictEqual(events.messages.at(-1), { type: 'resync', reason: 'logIdChanged' });

		const listed = await first.request('project/list', {});
		assert.deepStrictEqual(listed.projects.map(project => project.id), [one], 'projects outlive wispd');
	});
});

/** Stops the `serve` that attach started, through its lock file (decision record 0009), and waits for it to go. */
async function stopDaemon(dataDir: string): Promise<void> {
	const lock = join(dataDir, 'wispd.lock');
	let pid: number;
	try {
		pid = Number((await fs.readFile(lock, 'utf8')).trim());
	} catch {
		return;
	}
	if (!pid) {
		return;
	}
	try {
		process.kill(pid, 'SIGTERM');
	} catch {
		return;
	}
	for (let i = 0; i < 200; i++) {
		try {
			process.kill(pid, 0);
		} catch {
			return;
		}
		await new Promise(resolve => setTimeout(resolve, 50));
	}
	throw new Error(`wispd ${pid} did not stop within 10 s`);
}
