/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { WispdState, WispdSubscriptionMessage } from '../../common/wispd.js';
import { IWispdTransportFactory } from '../../common/wispdClient.js';
import { WispdHostConnection } from '../../common/wispdHostConnection.js';
import { FakeTimers, FakeWispd, settle } from './wispdTestUtils.js';

suite('WispdHostConnection', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	let local: FakeWispd;
	let remote: FakeWispd;
	let timers: FakeTimers;
	let host: 'local' | 'mac-mini';

	setup(() => {
		local = new FakeWispd();
		remote = new FakeWispd();
		remote.logId = 'remote-log';
		timers = new FakeTimers();
		host = 'local';
	});

	function factory(): IWispdTransportFactory {
		return host === 'local'
			? local
			: { command: 'ssh -- mac-mini wispd attach', create: () => remote.create() };
	}

	function createConnection(): { connection: WispdHostConnection; states: WispdState[] } {
		const connection = store.add(new WispdHostConnection(factory, { client: { name: 'wisp', version: '0.1.0' }, timers }, new NullLogger()));
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
});
