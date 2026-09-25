/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { CancellationTokenSource } from '../../../../base/common/cancellation.js';
import { isCancellationError } from '../../../../base/common/errors.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { WispdError, WispdState, WispdSubscriptionMessage, WispdUnavailableError, describeIncompatible } from '../../common/wispd.js';
import { WispdClient } from '../../common/wispdClient.js';
import { Project } from '../../common/wispProtocol.js';
import { FakeTimers, FakeWispd, settle } from './wispdTestUtils.js';

function project(id: string): Project {
	return { id, name: id, repoPath: `/src/${id}`, createdAt: '2026-09-24T00:00:00Z', updatedAt: '2026-09-24T00:00:00Z' };
}

/** Reads the state without the narrowing that an earlier assertion on `client.state` leaves. */
function stateOf(client: WispdClient): WispdState {
	return client.state;
}

suite('WispdClient', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	let wispd: FakeWispd;
	let timers: FakeTimers;
	let states: WispdState[];

	setup(() => {
		wispd = new FakeWispd();
		timers = new FakeTimers();
		states = [];
	});

	function createClient(): WispdClient {
		const client = store.add(new WispdClient(wispd, { client: { name: 'wisp', version: '0.1.0' }, timers }, new NullLogger()));
		store.add(client.onDidChangeState(state => states.push(state)));
		return client;
	}

	async function connect(): Promise<WispdClient> {
		const client = createClient();
		client.start();
		await settle();
		assert.strictEqual(client.state.kind, 'connected');
		return client;
	}

	test('waits to be started', async () => {
		const client = createClient();
		await timers.advance(60_000);
		assert.strictEqual(wispd.transports.length, 0);
		assert.deepStrictEqual(client.state, { kind: 'disconnected', command: 'wispd attach', reason: 'notStarted', message: 'Not connected yet.' });
	});

	test('handshake sends the protocol range and client, and reports what wispd supports', async () => {
		wispd.capabilities = { agents: {} };
		const client = await connect();

		assert.deepStrictEqual(wispd.current.sent[0], {
			jsonrpc: '2.0',
			id: 1,
			method: 'initialize',
			params: { protocol: { min: 1, max: 1 }, client: { name: 'wisp', version: '0.1.0' }, capabilities: {} },
		});
		assert.deepStrictEqual(states.map(state => state.kind), ['connecting', 'connected']);
		assert.deepStrictEqual(client.state, {
			kind: 'connected',
			command: 'wispd attach',
			wispd: '0.1.0',
			protocol: 1,
			logId: 'log-1',
			capabilities: { agents: {} },
			maxFrameBytes: 8388608,
		});
	});

	test('incompatibleProtocol stops retrying until retry, and names the side to update', async () => {
		wispd.incompatible = { min: 2, max: 3 };
		const client = createClient();
		client.start();
		await settle();

		const state = stateOf(client);
		assert.strictEqual(state.kind, 'incompatible');
		assert.deepStrictEqual({ update: state.update, wispd: state.wispd, requested: state.requested, supported: state.supported }, {
			update: 'editor',
			wispd: '9.0.0',
			requested: { min: 1, max: 1 },
			supported: { min: 2, max: 3 },
		});
		assert.ok(wispd.current.disposed, 'the attach process is stopped');

		await timers.advance(120_000);
		assert.strictEqual(wispd.transports.length, 1, 'no retries while incompatible');
		await assert.rejects(client.request('project/list', {}), WispdUnavailableError);

		wispd.incompatible = undefined;
		client.retry();
		await settle();
		assert.strictEqual(wispd.transports.length, 2);
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('an older wispd is the side to update', async () => {
		wispd.incompatible = { min: 0, max: 0 };
		const client = createClient();
		client.start();
		await settle();

		const state = stateOf(client);
		assert.strictEqual(state.kind, 'incompatible');
		assert.strictEqual(state.update, 'wispd');
		assert.match(describeIncompatible(state, 'mac-mini'), /^Update wisp on mac-mini/);
	});

	test('reconnects with backoff from 1 s to 10 s, and resets it after a handshake', async () => {
		const client = await connect();
		wispd.answering = false;

		const delays: number[] = [];
		for (let i = 0; i < 6; i++) {
			wispd.current.close({ reason: 'unreachable', message: 'never reached wispd', exitCode: 4, stderr: 'wispd attach: timed out' });
			const state = stateOf(client);
			assert.strictEqual(state.kind, 'disconnected');
			assert.strictEqual(state.reason, 'unreachable');
			assert.strictEqual(state.exitCode, 4);
			assert.strictEqual(state.stderr, 'wispd attach: timed out');
			delays.push(state.retryAt! - timers.now());
			const transports = wispd.transports.length;
			await timers.advance(state.retryAt! - timers.now() - 1);
			assert.strictEqual(wispd.transports.length, transports, 'nothing before the delay');
			await timers.advance(1);
			assert.strictEqual(wispd.transports.length, transports + 1);
		}
		assert.deepStrictEqual(delays, [1000, 2000, 4000, 8000, 10000, 10000]);

		wispd.answering = true;
		wispd.current.close();
		await timers.advance(10_000);
		assert.strictEqual(client.state.kind, 'connected');
		wispd.current.close();
		const state = stateOf(client);
		assert.strictEqual(state.kind, 'disconnected');
		assert.strictEqual(state.retryAt! - timers.now(), 1000);
	});

	test('a failed spawn is retried', async () => {
		wispd.spawnError = new Error('ENOENT');
		const client = createClient();
		client.start();
		const state = stateOf(client);
		assert.strictEqual(state.kind, 'disconnected');
		assert.strictEqual(state.reason, 'spawnFailed');

		wispd.spawnError = undefined;
		await timers.advance(1000);
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('sends host/health every 30 s and on wake', async () => {
		const client = await connect();
		await timers.advance(29_999);
		assert.strictEqual(wispd.current.requests('host/health').length, 0);
		await timers.advance(1);
		assert.strictEqual(wispd.current.requests('host/health').length, 1);
		await timers.advance(30_000);
		assert.strictEqual(wispd.current.requests('host/health').length, 2);

		client.onWake();
		await settle();
		assert.strictEqual(wispd.current.requests('host/health').length, 3);
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('10 s without bytes while a request is outstanding kills the child; any bytes reset the timer', async () => {
		const client = await connect();
		await timers.advance(60_000);
		assert.strictEqual(client.state.kind, 'connected', 'silence with nothing outstanding is fine');

		wispd.answering = false;
		client.onWake();
		await timers.advance(9_000);
		wispd.current.receiveData();
		await timers.advance(9_999);
		assert.strictEqual(client.state.kind, 'connected');
		const transport = wispd.current;
		await timers.advance(1);

		const state = stateOf(client);
		assert.strictEqual(state.kind, 'disconnected');
		assert.strictEqual(state.reason, 'timedOut');
		assert.ok(transport.disposed);
		assert.strictEqual(state.retryAt! - timers.now(), 1000);

		wispd.answering = true;
		await timers.advance(1000);
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('sending requests does not reset the liveness timer while wispd stays silent', async () => {
		const client = await connect();
		wispd.answering = false;
		client.request('project/list', {}).catch(() => { });
		await settle();
		assert.strictEqual(wispd.current.requests('project/list').length, 1);
		await timers.advance(6_000);
		client.request('project/list', {}).catch(() => { });
		const cts = new CancellationTokenSource();
		client.request('project/list', {}, cts.token).catch(() => { });
		await settle();
		cts.cancel();
		cts.dispose();
		await timers.advance(3_999);
		assert.strictEqual(client.state.kind, 'connected');
		await timers.advance(1);

		const state = stateOf(client);
		assert.strictEqual(state.kind, 'disconnected');
		assert.strictEqual(state.reason, 'timedOut');
		assert.strictEqual(timers.now(), 10_000, '10 s after the first request');
	});

	test('the handshake gets 20 s, since attach may be starting wispd', async () => {
		wispd.answering = false;
		const client = createClient();
		client.start();
		await timers.advance(19_999);
		assert.strictEqual(client.state.kind, 'connecting');
		await timers.advance(1);
		assert.strictEqual(client.state.kind, 'disconnected');
	});

	test('wake reconnects at once while waiting out the backoff', async () => {
		const client = await connect();
		wispd.current.close();
		wispd.current.close();
		assert.strictEqual(client.state.kind, 'disconnected');
		client.onWake();
		await settle();
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('events arrive once, in order, and resume from the last seq after a reconnect', async () => {
		wispd.emit({ kind: 'project.created', project: project('a') });
		const client = await connect();

		const messages: WispdSubscriptionMessage[] = [];
		store.add(client.subscribe({ after: 0 }, message => messages.push(message)));
		await settle();
		wispd.emit({ kind: 'project.created', project: project('b') });

		wispd.current.close();
		wispd.emit({ kind: 'project.created', project: project('c') });
		await timers.advance(1000);

		assert.deepStrictEqual(wispd.current.requests('events/subscribe').map(request => request.params), [{ after: 2 }]);
		wispd.current.receive({ jsonrpc: '2.0', method: 'events/event', params: { subscription: 'sub-2', seq: 2, time: '2026-09-24T00:00:00Z', event: { kind: 'project.created', project: project('b') } } });
		wispd.current.receive({ jsonrpc: '2.0', method: 'events/event', params: { subscription: 'sub-9', seq: 4, time: '2026-09-24T00:00:00Z', event: { kind: 'project.created', project: project('d') } } });
		assert.deepStrictEqual(messages.map(message => message.type === 'event' ? message.event.seq : message.type), [1, 2, 3], 'no repeats, and nothing for other subscriptions');
		const first = messages[0];
		assert.deepStrictEqual(first, { type: 'event', event: { seq: 1, time: '2026-09-24T00:00:00Z', event: { kind: 'project.created', project: project('a') } } });
	});

	test('a subscription made while disconnected starts once connected', async () => {
		wispd.answering = false;
		const client = createClient();
		const messages: WispdSubscriptionMessage[] = [];
		store.add(client.subscribe({ after: 0, project: 'p' }, message => messages.push(message)));
		wispd.emit({ kind: 'project.created', project: project('x') }, 'p');
		wispd.answering = true;
		wispd.current.close();
		await timers.advance(1000);

		assert.deepStrictEqual(wispd.current.requests('events/subscribe').map(request => request.params), [{ after: 0, project: 'p' }]);
		assert.deepStrictEqual(messages.map(message => message.type), ['event']);
		assert.strictEqual(client.state.kind, 'connected');
	});

	test('a new logId after a reconnect ends subscriptions with resync', async () => {
		const client = await connect();
		const messages: WispdSubscriptionMessage[] = [];
		store.add(client.subscribe({ after: 0 }, message => messages.push(message)));
		await settle();

		wispd.restart('log-2');
		wispd.current.close();
		await timers.advance(1000);

		assert.deepStrictEqual(messages, [{ type: 'resync', reason: 'logIdChanged' }]);
		assert.strictEqual(wispd.current.requests('events/subscribe').length, 0);
	});

	test('a subscription for an old logId resyncs at once', async () => {
		const client = await connect();
		const messages: WispdSubscriptionMessage[] = [];
		store.add(client.subscribe({ after: 5, logId: 'log-0' }, message => messages.push(message)));
		await settle();
		assert.deepStrictEqual(messages, [{ type: 'resync', reason: 'logIdChanged' }]);
	});

	test('resyncRequired ends the subscription with resync', async () => {
		const client = await connect();
		wispd.resyncRequired = true;
		const messages: WispdSubscriptionMessage[] = [];
		store.add(client.subscribe({ after: 0 }, message => messages.push(message)));
		await settle();
		assert.deepStrictEqual(messages, [{ type: 'resync', reason: 'resyncRequired' }]);
	});

	test('disposing a subscription unsubscribes', async () => {
		const client = await connect();
		const messages: WispdSubscriptionMessage[] = [];
		const subscription = client.subscribe({ after: 0 }, message => messages.push(message));
		await settle();
		subscription.dispose();
		await settle();
		assert.deepStrictEqual(wispd.current.requests('events/unsubscribe').map(request => request.params), [{ subscription: 'sub-1' }]);
		wispd.emit({ kind: 'project.created', project: project('a') });
		assert.deepStrictEqual(messages, []);
	});

	test('a request lost to a disconnect is resent with the same id after the reconnect', async () => {
		const client = await connect();
		wispd.answering = false;
		const params = { id: '0192f0c4-0000-7000-8000-000000000001', name: 'wisp', repoPath: '/src/wisp' };
		const created = client.request('project/create', params);
		await settle();
		assert.deepStrictEqual(wispd.current.requests('project/create').map(request => request.params), [params]);

		wispd.answering = true;
		wispd.current.close();
		await timers.advance(1000);

		assert.deepStrictEqual(wispd.current.requests('project/create').map(request => request.params), [params]);
		assert.deepStrictEqual((await created).project.id, params.id);
		assert.strictEqual(wispd.projects.length, 1);
	});

	test('a request made while disconnected waits for the connection, up to 30 s', async () => {
		const client = await connect();
		wispd.spawnError = new Error('ENOENT');
		wispd.current.close();

		const errors: unknown[] = [];
		client.request('project/list', {}).catch(error => errors.push(error));
		await timers.advance(29_999);
		assert.strictEqual(errors.length, 0);
		await timers.advance(1);
		assert.ok(errors[0] instanceof WispdUnavailableError, String(errors[0]));
		assert.match((errors[0] as Error).message, /not reachable through wispd attach/);
	});

	test('wispd errors keep their kind', async () => {
		const client = await connect();
		const error = await client.request('host/version', {}).then(() => undefined, e => e);
		assert.ok(error instanceof WispdError);
		assert.strictEqual(error.code, -32601);
		assert.strictEqual(error.kind, undefined);

		const id = '0192f0c4-0000-7000-8000-000000000002';
		await client.request('project/create', { id, name: 'a', repoPath: '/src/a' });
		const conflict = await client.request('project/create', { id, name: 'b', repoPath: '/src/a' }).then(() => undefined, e => e);
		assert.ok(conflict instanceof WispdError);
		assert.strictEqual(conflict.code, -32000);
		assert.strictEqual(conflict.kind, 'idConflict');
	});

	test('cancelling a request sends $/cancelRequest with its id', async () => {
		const client = await connect();
		wispd.answering = false;
		const cts = new CancellationTokenSource();
		const listed = client.request('project/list', {}, cts.token);
		await settle();
		const id = wispd.current.requests('project/list')[0].id;

		cts.cancel();
		const error = await listed.then(() => undefined, e => e);
		assert.ok(isCancellationError(error));
		assert.deepStrictEqual(wispd.current.requests('$/cancelRequest').map(request => request.params), [{ id }]);
		cts.dispose();
	});

	test('skips lines that are not JSON', async () => {
		const client = createClient();
		client.start();
		wispd.current.receiveLine('Last login: today');
		wispd.current.receiveLine('');
		await settle();
		assert.strictEqual(client.state.kind, 'connected');
	});
});
