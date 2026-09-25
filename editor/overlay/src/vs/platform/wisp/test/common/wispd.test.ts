/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { CancellationToken } from '../../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../../base/common/event.js';
import { IChannel } from '../../../../base/parts/ipc/common/ipc.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { IWispdService, IWispdSubscribeOptions, WispdChannel, WispdChannelClient, WispdError, WispdMethod, WispdState, WispdSubscriptionMessage, WispdUnavailableError } from '../../common/wispd.js';
import { generateUuidV7 } from '../../common/uuidv7.js';

suite('WispdChannel', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	function client(service: Partial<IWispdService>): WispdChannelClient {
		const server = new WispdChannel(service as IWispdService);
		// IPC passes plain data, so the round trip goes through JSON as the real channel would.
		const channel: IChannel = {
			call: async (command, arg, token) => JSON.parse(JSON.stringify(await server.call(undefined, command, arg, token)) ?? 'null'),
			listen: (event, arg) => server.listen(undefined, event, arg),
		};
		return new WispdChannelClient(channel);
	}

	test('results come through', async () => {
		const service = client({
			async request<M extends WispdMethod>(method: M) {
				return { projects: [], seq: method === 'project/list' ? 7 : 0 } as never;
			},
		});
		assert.deepStrictEqual(await service.request('project/list', {}), { projects: [], seq: 7 });
	});

	test('wispd errors keep their code, kind, and detail across IPC', async () => {
		const service = client({
			async request() {
				throw new WispdError(-32000, 'project not found', 'projectNotFound', { id: 'x' });
			},
		});
		const error = await service.request('project/list', {}).then(() => undefined, e => e);
		assert.ok(error instanceof WispdError);
		assert.deepStrictEqual([error.code, error.message, error.kind, error.detail], [-32000, 'project not found', 'projectNotFound', { id: 'x' }]);
	});

	test('unavailable errors keep their type across IPC', async () => {
		const service = client({
			async request() {
				throw new WispdUnavailableError('wispd is not reachable');
			},
		});
		await assert.rejects(service.request('project/list', {}), (error: unknown) => error instanceof WispdUnavailableError && error.message === 'wispd is not reachable');
	});

	test('the token reaches the service', async () => {
		let received: CancellationToken | undefined;
		const server = new WispdChannel({
			async request(_method: WispdMethod, _params: unknown, token?: CancellationToken) {
				received = token;
				return {} as never;
			},
		} as Partial<IWispdService> as IWispdService);
		await server.call(undefined, 'request', ['project/list', {}], CancellationToken.None);
		assert.strictEqual(received, CancellationToken.None);
	});

	test('state and subscriptions are events', async () => {
		const states = store.add(new Emitter<WispdState>());
		const events = store.add(new Emitter<WispdSubscriptionMessage>());
		let options: IWispdSubscribeOptions | undefined;
		const service = client({
			onDidChangeState: states.event,
			subscribe(o: IWispdSubscribeOptions): Event<WispdSubscriptionMessage> {
				options = o;
				return events.event;
			},
		});

		const seen: string[] = [];
		store.add(service.onDidChangeState(state => seen.push(state.kind)));
		store.add(service.subscribe({ after: 3, project: 'p' })(message => seen.push(message.type)));
		states.fire({ kind: 'connecting', command: 'wispd attach', attempt: 1 });
		events.fire({ type: 'resync', reason: 'resyncRequired' });

		assert.deepStrictEqual(options, { after: 3, project: 'p' });
		assert.deepStrictEqual(seen, ['connecting', 'resync']);
	});
});

suite('generateUuidV7', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('is a version 7, variant 1 UUID with the time in front', () => {
		const id = generateUuidV7(0x0192f0c4a1b2);
		assert.match(id, /^0192f0c4-a1b2-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
	});

	test('differs between calls in the same millisecond', () => {
		assert.notStrictEqual(generateUuidV7(1), generateUuidV7(1));
	});
});
