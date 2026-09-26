/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { CancellationToken } from '../../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../../base/common/event.js';
import { IChannel } from '../../../../base/parts/ipc/common/ipc.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { TestConfigurationService } from '../../../configuration/test/common/testConfigurationService.js';
import { IWispdService, IWispdSubscribeOptions, WispdChannel, WispdChannelClient, WispdError, WispdMethod, WispdState, WispdSubscriptionMessage, WispdUnavailableError } from '../../common/wispd.js';
import { WISP_HOST_SETTING, wispdTarget } from '../../common/wispdConfiguration.js';
import { generateUuidV7 } from '../../common/uuidv7.js';

suite('WispdChannel', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	function client(service: Partial<IWispdService>, configuration = new TestConfigurationService()): WispdChannelClient {
		const server = new WispdChannel({ onDidChangeState: Event.None, ...service } as IWispdService);
		// IPC passes plain data, so the round trip goes through JSON as the real channel would.
		const channel: IChannel = {
			call: async (command, arg, token) => JSON.parse(JSON.stringify(await server.call(undefined, command, arg, token)) ?? 'null'),
			listen: (event, arg) => server.listen(undefined, event, arg),
		};
		return store.add(new WispdChannelClient(channel, configuration));
	}

	function switchHost(configuration: TestConfigurationService, host: string): void {
		configuration.setUserConfiguration(WISP_HOST_SETTING, host);
		configuration.onDidChangeConfigurationEmitter.fire({ affectedKeys: new Set([WISP_HOST_SETTING]), affectsConfiguration: (key: string) => key === WISP_HOST_SETTING } as never);
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

	test('every request and state query names the window\'s host (#219)', async () => {
		const targets: Array<string | undefined> = [];
		const configuration = new TestConfigurationService({ [WISP_HOST_SETTING]: 'mac-mini' });
		const service = client({
			async request(_method: WispdMethod, _params: unknown, _token?: CancellationToken, target?: string) {
				targets.push(target);
				return {} as never;
			},
			async getState(target?: string) {
				targets.push(target);
				return { kind: 'connecting', command: 'ssh', attempt: 1, target };
			},
		}, configuration);

		await service.request('project/list', {});
		await service.getState();
		assert.deepStrictEqual(targets, [wispdTarget('mac-mini', ''), wispdTarget('mac-mini', '')]);
	});

	test('a state for another host shows as connecting to the window\'s host', async () => {
		const states = store.add(new Emitter<WispdState>());
		const configuration = new TestConfigurationService({ [WISP_HOST_SETTING]: 'mac-mini' });
		const service = client({ onDidChangeState: states.event }, configuration);
		const seen: WispdState[] = [];
		store.add(service.onDidChangeState(state => seen.push(state)));

		const connected: WispdState = { kind: 'connected', command: '/Applications/Wisp.app/bin/wispd attach', wispd: '0.1.0', protocol: 1, logId: 'log-1', capabilities: {}, maxFrameBytes: 8388608, target: 'local' };
		states.fire(connected);
		const onMacMini: WispdState = { ...connected, command: 'ssh -T -- mac-mini wispd attach', target: wispdTarget('mac-mini', '') };
		states.fire(onMacMini);

		assert.deepStrictEqual(seen, [
			{ kind: 'connecting', command: 'ssh -- mac-mini wispd attach', attempt: 1, target: wispdTarget('mac-mini', '') },
			onMacMini,
		]);
	});

	test('a host change in the window shows as connecting at once, and asks the shared process to catch up', async () => {
		const states = store.add(new Emitter<WispdState>());
		const asked: Array<string | undefined> = [];
		const configuration = new TestConfigurationService({ [WISP_HOST_SETTING]: 'local' });
		const connected: WispdState = { kind: 'connected', command: 'wispd attach', wispd: '0.1.0', protocol: 1, logId: 'log-1', capabilities: {}, maxFrameBytes: 8388608, target: 'local' };
		const service = client({
			onDidChangeState: states.event,
			async getState(target?: string) {
				asked.push(target);
				return connected;
			},
		}, configuration);
		states.fire(connected);
		const seen: string[] = [];
		store.add(service.onDidChangeState(state => seen.push(`${state.kind} ${state.target}`)));

		switchHost(configuration, 'mac-mini');
		await Promise.resolve();

		assert.deepStrictEqual(seen, [`connecting ${wispdTarget('mac-mini', '')}`]);
		assert.deepStrictEqual(asked, [wispdTarget('mac-mini', '')]);
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
