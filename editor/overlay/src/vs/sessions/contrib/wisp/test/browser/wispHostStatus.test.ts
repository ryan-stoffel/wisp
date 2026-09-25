/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { WispdDisconnectReason, WispdState } from '../../../../../platform/wisp/common/wispd.js';
import { describeHostStatus, IWispHostContext, IWispHostStatus, WispHostProblem } from '../../browser/wispHostStatus.js';
import { connected, connecting, disconnected, incompatible, SSH_COMMAND } from './wispHostTestUtils.js';

const NO_HOST_BODY = 'The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.';
const LOCAL: IWispHostContext = { host: 'this Mac', remote: false, wasConnected: false };
const REMOTE: IWispHostContext = { host: 'mac-mini', remote: true, wasConnected: false };

function remote(state: WispdState, context: Partial<IWispHostContext> = {}): IWispHostStatus {
	return describeHostStatus(state, { ...REMOTE, ...context });
}

suite('wisp: host status', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	suite('chip', () => {

		test('connected shows the host with a filled dot, and names the state for screen readers', () => {
			const status = describeHostStatus(connected(), LOCAL);
			assert.deepStrictEqual(
				{ kind: status.kind, mark: status.mark, host: status.host, showState: status.showState, ariaLabel: status.ariaLabel, wispd: status.wispd },
				{ kind: 'connected', mark: 'connected', host: 'this Mac', showState: false, ariaLabel: 'Host: this Mac, connected', wispd: '0.1.0' },
			);
			assert.strictEqual(remote(connected(SSH_COMMAND)).ariaLabel, 'Host: mac-mini, connected');
		});

		test('before anything connects: not connected, then connecting, each with its own mark and words', () => {
			const idle = describeHostStatus(disconnected('notStarted'), LOCAL);
			assert.deepStrictEqual([idle.kind, idle.mark, idle.state, idle.ariaLabel], ['notConnected', 'idle', 'not connected', 'Host: this Mac, not connected']);
			const trying = remote(connecting(1, SSH_COMMAND));
			assert.deepStrictEqual([trying.kind, trying.mark, trying.state, trying.ariaLabel], ['connecting', 'connecting', 'connecting', 'Host: mac-mini, connecting']);
		});

		test('every problem is a diamond with the state in words next to the host', () => {
			const reasons: WispdDisconnectReason[] = ['spawnFailed', 'invalidHost', 'unreachable', 'noRoute', 'authFailed', 'hostKeyUnknown', 'hostKeyChanged', 'wispdNotFound', 'exited', 'timedOut', 'frameTooLarge', 'protocolError'];
			for (const reason of reasons) {
				const status = remote(disconnected(reason, SSH_COMMAND));
				assert.strictEqual(status.kind, 'error', reason);
				assert.strictEqual(status.mark, 'error', reason);
				assert.strictEqual(status.showState, true, reason);
				assert.strictEqual(status.ariaLabel, `Host: mac-mini, ${status.state}`, reason);
			}
			assert.strictEqual(remote(disconnected('hostKeyChanged', SSH_COMMAND)).ariaLabel, 'Host: mac-mini, host key changed');
			assert.strictEqual(remote(incompatible('wispd')).ariaLabel, 'Host: mac-mini, needs an update');
		});
	});

	suite('no-host view', () => {

		test('with no host yet, it shows the M0 copy and a composer that is not connected', () => {
			for (const state of [disconnected('notStarted'), connecting(1)]) {
				const status = describeHostStatus(state, LOCAL);
				assert.deepStrictEqual(
					{ heading: status.heading, body: status.body, placeholder: status.placeholder, actions: status.actions },
					{ heading: 'No host connected', body: NO_HOST_BODY, placeholder: 'Not connected to a host', actions: [] },
				);
			}
		});

		test('losing a host that was connected uses the spec\'s copy and offers Retry now, Switch host, and Show log', () => {
			for (const reason of ['noRoute', 'exited', 'timedOut', 'protocolError', 'frameTooLarge'] as const) {
				const status = remote(disconnected(reason, SSH_COMMAND), { wasConnected: true });
				assert.deepStrictEqual(
					{ heading: status.heading, body: status.body, placeholder: status.placeholder, actions: status.actions, state: status.state },
					{
						heading: 'Lost connection to mac-mini.',
						body: 'wisp retries every 10 seconds. Agents already running on the host keep going.',
						placeholder: 'Reconnect to send messages',
						actions: ['retry', 'switchHost', 'showLog'],
						state: 'offline',
					},
					reason,
				);
			}
		});

		test('each reason a host never connected has its own heading', () => {
			const headings = Object.fromEntries((['noRoute', 'authFailed', 'hostKeyUnknown', 'hostKeyChanged', 'wispdNotFound', 'unreachable', 'spawnFailed', 'invalidHost', 'protocolError', 'exited'] as const)
				.map(reason => [reason, remote(disconnected(reason, SSH_COMMAND)).heading]));
			assert.deepStrictEqual(headings, {
				noRoute: 'Can\'t reach mac-mini.',
				authFailed: 'ssh couldn\'t sign in to mac-mini.',
				hostKeyUnknown: 'ssh doesn\'t know mac-mini\'s host key yet.',
				hostKeyChanged: 'mac-mini\'s host key has changed.',
				wispdNotFound: 'wispd isn\'t installed on mac-mini.',
				unreachable: 'wispd isn\'t running on mac-mini.',
				spawnFailed: 'wisp couldn\'t run ssh.',
				invalidHost: 'The Wisp: Host setting isn\'t a usable host.',
				protocolError: 'wispd on mac-mini sent something wisp can\'t read.',
				exited: 'Can\'t connect to mac-mini.',
			});
		});

		test('every problem asks to reconnect in the composer', () => {
			for (const reason of ['noRoute', 'authFailed', 'hostKeyChanged', 'invalidHost', 'spawnFailed'] as const) {
				assert.strictEqual(remote(disconnected(reason, SSH_COMMAND)).placeholder, 'Reconnect to send messages', reason);
			}
			assert.strictEqual(remote(incompatible('editor')).placeholder, 'Reconnect to send messages');
		});

		test('a changed host key warns of interception and never suggests accepting the key', () => {
			const status = remote(disconnected('hostKeyChanged', SSH_COMMAND));
			assert.match(status.body, /intercepting the connection/);
			assert.match(status.body, /won't connect/);
			assert.deepStrictEqual(status.actions, ['retry', 'switchHost', 'showLog']);
			for (const reason of ['hostKeyChanged', 'hostKeyUnknown'] as const) {
				const copy = remote(disconnected(reason, SSH_COMMAND));
				assert.doesNotMatch(`${copy.heading} ${copy.body}`, /accept/i, reason);
			}
		});

		test('the spec\'s copy for wispd not running on a host', () => {
			const status = remote(disconnected('unreachable', SSH_COMMAND));
			assert.strictEqual(status.body, 'Start it on the host, then retry.');
		});

		test('an invalid Wisp: Host shows why, and offers Switch host and Open Settings instead of Retry', () => {
			const status = remote(disconnected('invalidHost', 'ssh -- -x wispd attach', 'wisp.host ("-x") is not a valid ssh destination: it starts with "-".'));
			assert.strictEqual(status.body, 'wisp.host ("-x") is not a valid ssh destination: it starts with "-".');
			assert.deepStrictEqual(status.actions, ['switchHost', 'openSettings', 'showLog']);
		});

		test('this Mac and a remote host word a failed start differently', () => {
			const local = describeHostStatus(disconnected('spawnFailed', undefined, 'Could not run wispd: ENOENT'), LOCAL);
			assert.strictEqual(local.heading, 'wisp couldn\'t start wispd.');
			assert.match(local.body, /^The wispd that comes with Wisp is missing or can't run\. .*ENOENT$/);
			assert.strictEqual(remote(disconnected('spawnFailed', SSH_COMMAND, 'Could not run ssh: ENOENT')).body, 'Could not run ssh: ENOENT');
		});

		test('an incompatible wispd names the side to update', () => {
			const wispd = remote(incompatible('wispd'));
			assert.strictEqual(wispd.heading, 'wispd on mac-mini needs an update');
			assert.match(wispd.body, /^Update wisp on mac-mini/);
			assert.strictEqual(remote(incompatible('editor')).heading, 'Wisp needs an update to use mac-mini');
		});

		test('while the client retries after a problem, the problem\'s copy stays, marked as retrying', () => {
			const lastProblem: WispHostProblem = disconnected('noRoute', SSH_COMMAND);
			const retrying = remote(connecting(3, SSH_COMMAND), { lastProblem });
			const problem = remote(lastProblem);
			assert.strictEqual(retrying.retrying, true);
			assert.deepStrictEqual({ ...retrying, retrying: false }, problem);
		});
	});
});
