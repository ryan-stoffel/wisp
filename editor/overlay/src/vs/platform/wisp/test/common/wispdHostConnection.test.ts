/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { CancellationToken } from '../../../../base/common/cancellation.js';
import { IChannel } from '../../../../base/parts/ipc/common/ipc.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { TestConfigurationService } from '../../../configuration/test/common/testConfigurationService.js';
import { NullLogger } from '../../../log/common/log.js';
import { IWispdService, IWispdSubscribeOptions, WispdChannel, WispdChannelClient, WispdMethod, WispdState, WispdSubscriptionMessage, WispdUnavailableError } from '../../common/wispd.js';
import { IWispdTransportFactory } from '../../common/wispdClient.js';
import { WISP_HOST_SETTING, wispdTarget } from '../../common/wispdConfiguration.js';
import { IWispdHost, WispdHostConnection } from '../../common/wispdHostConnection.js';
import { WispRequests } from '../../common/wispProtocol.js';
import { FakeTimers, FakeWispd, settle } from './wispdTestUtils.js';

suite('WispdHostConnection', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	let local: FakeWispd;
	let remote: FakeWispd;
	let timers: FakeTimers;
	/** What this process's settings say. */
	let host: 'local' | 'mac-mini';
	/** What settings.json says, which this process reads only when it reloads. */
	let savedHost: 'local' | 'mac-mini';
	let reloads: number;

	setup(() => {
		local = new FakeWispd();
		remote = new FakeWispd();
		remote.logId = 'remote-log';
		timers = new FakeTimers();
		host = 'local';
		savedHost = 'local';
		reloads = 0;
	});

	function factory(): IWispdTransportFactory {
		return host === 'local'
			? local
			: { command: 'ssh -- mac-mini wispd attach', create: () => remote.create() };
	}

	function resolveHost(): IWispdHost {
		return { factory: factory(), target: wispdTarget(host, '') };
	}

	async function reloadSettings(): Promise<void> {
		reloads++;
		await Promise.resolve();
		host = savedHost;
	}

	function createConnection(): { connection: WispdHostConnection; states: WispdState[] } {
		const connection = store.add(new WispdHostConnection(resolveHost, reloadSettings, { client: { name: 'wisp', version: '0.1.0' }, timers }, new NullLogger()));
		const states: WispdState[] = [];
		store.add(connection.onDidChangeState(state => states.push(state)));
		return { connection, states };
	}

	test('does nothing until started, and keeps its client while the host is the same', async () => {
		const { connection } = createConnection();
		assert.strictEqual(connection.update(), false);
		await settle();
		assert.strictEqual(local.transports.length, 0);
		assert.strictEqual(connection.state.kind, 'disconnected');
	});

	test('a changed host replaces the client and connects to the new host at once', async () => {
		const { connection, states } = createConnection();
		connection.start();
		await settle();
		assert.strictEqual(connection.state.kind, 'connected');

		host = 'mac-mini';
		assert.strictEqual(connection.update(), true);
		await settle();

		assert.strictEqual(local.current.disposed, true);
		assert.strictEqual(remote.transports.length, 1);
		assert.deepStrictEqual(
			states.map(state => `${state.kind} ${state.command}`),
			[
				'connecting wispd attach',
				'connected wispd attach',
				'disconnected ssh -- mac-mini wispd attach',
				'connecting ssh -- mac-mini wispd attach',
				'connected ssh -- mac-mini wispd attach',
			],
		);
		assert.strictEqual(connection.state.kind === 'connected' && connection.state.logId, 'remote-log');
	});

	test('a changed host before anything started does not connect', async () => {
		const { connection } = createConnection();
		host = 'mac-mini';
		connection.update();
		await settle();
		assert.strictEqual(remote.transports.length, 0);
		assert.strictEqual(connection.state.command, 'ssh -- mac-mini wispd attach');
	});

	test('live subscriptions end with resync when the host changes, and get nothing from the new host', async () => {
		const { connection } = createConnection();
		const messages: WispdSubscriptionMessage[] = [];
		store.add(connection.subscribe({ after: 0 })(message => messages.push(message)));
		await settle();

		host = 'mac-mini';
		connection.update();
		await settle();
		remote.emit({ kind: 'project.created', project: { id: 'p1', name: 'p1', repoPath: '/src/p1', createdAt: '2026-09-25T00:00:00Z', updatedAt: '2026-09-25T00:00:00Z' } });
		await settle();

		assert.deepStrictEqual(messages, [{ type: 'resync', reason: 'logIdChanged' }]);
		assert.strictEqual(remote.current.requests('events/subscribe').length, 0);
	});

	test('a request waiting on the old host fails instead of going to the new one', async () => {
		local.answering = false;
		const { connection } = createConnection();
		const pending = connection.request('project/list', {});
		await settle();

		host = 'mac-mini';
		connection.update();
		await assert.rejects(pending);
		await settle();
		assert.strictEqual(remote.current.requests('project/list').length, 0);
	});

	test('every state names the target it is for', async () => {
		const { connection, states } = createConnection();
		connection.start();
		await settle();
		host = 'mac-mini';
		connection.update();
		await settle();

		assert.deepStrictEqual(
			[...new Set(states.map(state => `${state.command} ${state.target}`))],
			['wispd attach local', 'ssh -- mac-mini wispd attach ["mac-mini",""]'],
		);
		assert.strictEqual(connection.state.target, wispdTarget('mac-mini', ''));
	});

	test('a request for a host these settings have not caught up with reads them again and goes to that host (#219)', async () => {
		const { connection } = createConnection();
		connection.start();
		await settle();

		// The window saved mac-mini and saw it at once; this process's file watcher hasn't fired yet.
		savedHost = 'mac-mini';
		const created = await connection.request('project/create', { id: 'p1', name: 'p1', repoPath: '/src/p1' }, undefined, wispdTarget('mac-mini', ''));

		assert.strictEqual(created.project.id, 'p1');
		assert.deepStrictEqual(remote.projects.map(project => project.id), ['p1']);
		assert.deepStrictEqual(local.projects, []);
		assert.strictEqual(local.current.requests('project/create').length, 0);
		assert.strictEqual(connection.state.target, wispdTarget('mac-mini', ''));
	});

	test('a request whose host the settings still do not name is refused without being sent', async () => {
		const { connection } = createConnection();
		connection.start();
		await settle();

		await assert.rejects(
			connection.request('project/create', { id: 'p1', name: 'p1', repoPath: '/src/p1' }, undefined, wispdTarget('mac-mini', '')),
			(error: unknown) => error instanceof WispdUnavailableError,
		);
		assert.strictEqual(reloads, 1);
		assert.strictEqual(local.current.requests('project/create').length, 0);
		assert.strictEqual(remote.transports.length, 0);
	});

	test('a request for the host already configured goes straight out, without reading the settings again', async () => {
		const { connection } = createConnection();
		await connection.request('project/list', {}, undefined, wispdTarget('local', ''));
		assert.strictEqual(reloads, 0);
		assert.strictEqual(local.current.requests('project/list').length, 1);
	});

	test('getState with a new target catches up before it starts', async () => {
		const { connection } = createConnection();
		savedHost = 'mac-mini';
		const state = await connection.getState(wispdTarget('mac-mini', ''));
		await settle();

		assert.strictEqual(state.command, 'ssh -- mac-mini wispd attach');
		assert.strictEqual(local.transports.length, 0);
		assert.strictEqual(remote.transports.length, 1);
	});
});

/**
 * The window and the shared process together, right after a host switch: the window's settings
 * already name the new host, and the shared process's don't yet, because it sees settings.json
 * change only through a file watcher (#219).
 */
suite('Host switch between a window and the shared process', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	let local: FakeWispd;
	let remote: FakeWispd;
	let sharedHost: string;
	let savedHost: string;
	let windowConfiguration: TestConfigurationService;
	let connection: WispdHostConnection;
	let window: WispdChannelClient;

	setup(async () => {
		local = new FakeWispd();
		remote = new FakeWispd();
		remote.logId = 'remote-log';
		sharedHost = 'local';
		savedHost = 'local';

		connection = store.add(new WispdHostConnection(
			() => ({
				factory: sharedHost === 'local' ? local : { command: `ssh -- ${sharedHost} wispd attach`, create: () => remote.create() },
				target: wispdTarget(sharedHost, ''),
			}),
			async () => { sharedHost = savedHost; },
			{ client: { name: 'wisp', version: '0.1.0' }, timers: new FakeTimers() },
			new NullLogger(),
		));
		const service: IWispdService = {
			_serviceBrand: undefined,
			onDidChangeState: connection.onDidChangeState,
			getState: target => connection.getState(target),
			retry: async () => connection.retry(),
			request: <M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken, target?: string) => connection.request(method, params, token, target),
			subscribe: (options: IWispdSubscribeOptions) => connection.subscribe(options),
		};
		const server = new WispdChannel(service);
		const channel: IChannel = {
			call: async (command, arg, token) => JSON.parse(JSON.stringify(await server.call(undefined, command, arg, token)) ?? 'null'),
			listen: (event, arg) => server.listen(undefined, event, arg),
		};
		windowConfiguration = new TestConfigurationService({ [WISP_HOST_SETTING]: 'local' });
		window = store.add(new WispdChannelClient(channel, windowConfiguration));
		await window.getState();
		await settle();
	});

	/** The host menu: settings.json is saved, then the window sees the new value. The shared process doesn't yet. */
	async function switchWindowTo(host: string): Promise<void> {
		savedHost = host;
		await windowConfiguration.setUserConfiguration(WISP_HOST_SETTING, host);
		windowConfiguration.onDidChangeConfigurationEmitter.fire({ affectedKeys: new Set([WISP_HOST_SETTING]), affectsConfiguration: (key: string) => key === WISP_HOST_SETTING } as never);
	}

	test('a project created right after switching to an ssh host is stored by that host\'s wispd', async () => {
		assert.strictEqual((await window.getState()).kind, 'connected');

		await switchWindowTo('mac-mini');
		const { project } = await window.request('project/create', { id: 'p1', name: 'p1', repoPath: '/src/p1' });

		assert.strictEqual(project.id, 'p1');
		assert.deepStrictEqual(remote.projects.map(p => p.id), ['p1']);
		assert.deepStrictEqual(local.projects, []);
	});

	test('the window never shows the old host\'s connection as its new host', async () => {
		const seen: Array<{ kind: string; target?: string }> = [];
		store.add(window.onDidChangeState(state => seen.push({ kind: state.kind, target: state.target })));

		await switchWindowTo('mac-mini');
		await settle();

		const target = wispdTarget('mac-mini', '');
		assert.ok(seen.length > 0);
		assert.ok(seen.every(state => state.target === target), JSON.stringify(seen));
		assert.strictEqual(seen[0].kind, 'connecting');
		assert.strictEqual(seen[seen.length - 1].kind, 'connected');
		// The window's change alone moved the shared process, without its file watcher.
		assert.strictEqual(connection.state.target, target);
	});

	test('a window whose settings are behind the file has its requests refused, not sent to the other host', async () => {
		// Another window switched to mac-mini, and the shared process has already caught up.
		savedHost = 'mac-mini';
		sharedHost = 'mac-mini';
		connection.update();
		await settle();

		const states: string[] = [];
		store.add(window.onDidChangeState(state => states.push(`${state.kind} ${state.target}`)));
		await assert.rejects(window.request('project/create', { id: 'p1', name: 'p1', repoPath: '/src/p1' }), (error: unknown) => error instanceof WispdUnavailableError);
		await settle();
		assert.deepStrictEqual(remote.projects, []);
		assert.deepStrictEqual(local.projects, []);
		assert.strictEqual((await window.getState()).kind, 'connecting');
		assert.ok(states.every(state => state === 'connecting local'), JSON.stringify(states));
	});
});
