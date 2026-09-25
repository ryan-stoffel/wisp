/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { DisposableStore } from '../../../../../base/common/lifecycle.js';
import { NullLogService } from '../../../../../platform/log/common/log.js';
import { InMemoryStorageService, StorageScope } from '../../../../../platform/storage/common/storage.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import type { WispdState } from '../../../../../platform/wisp/common/wispd.js';
import type { AccountUsage, Capabilities, DetectedCli, KeyAccount } from '../../../../../platform/wisp/common/wispProtocol.js';
import {
	cliAccountLabel, cliLabel, formatCost, formatLimitDetail, formatPeriodValue, formatResetTime, formatTokenTotal, formatUsedPercent,
	keyAccountLabel, matchUsageAccount, providerLabel, resolveAccountLabel, windowLabel, WispAccountsService,
} from '../../browser/wispAccounts.js';
import { settle } from './wispAgentsTestServices.js';
import { connected, disconnected, TestWispdService } from './wispHostTestUtils.js';

function cli(overrides: Partial<DetectedCli> = {}): DetectedCli {
	return { cli: 'claude', installed: true, signedIn: true, plan: 'Max', ...overrides };
}

function keyAccount(overrides: Partial<KeyAccount> = {}): KeyAccount {
	return { id: 'key-1', provider: 'anthropic', label: 'Prod', createdAt: '2026-09-24T12:00:00Z', maskedKey: 'sk-ant-...abcd', ...overrides };
}

function connectedWith(capabilities: Capabilities): Extract<WispdState, { kind: 'connected' }> {
	const base = connected() as Extract<WispdState, { kind: 'connected' }>;
	return { ...base, capabilities };
}

function usage(overrides: Partial<AccountUsage> = {}): AccountUsage {
	return {
		accountId: 'claude',
		today: { inputTokens: 100, outputTokens: 50, cacheReadTokens: 0, cacheWriteTokens: 0 },
		week: { inputTokens: 1000, outputTokens: 500, cacheReadTokens: 0, cacheWriteTokens: 0 },
		limits: [],
		...overrides,
	};
}

suite('wisp: accounts', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	suite('labels', () => {

		test('a detected CLI is named by vendor, with the plan appended when known', () => {
			assert.strictEqual(cliLabel('claude'), 'Claude Code');
			assert.strictEqual(cliLabel('codex'), 'Codex');
			assert.strictEqual(cliLabel('cursor'), 'Cursor');
			assert.strictEqual(cliLabel('future-cli'), 'future-cli');
			assert.strictEqual(cliAccountLabel(cli({ cli: 'claude', plan: 'Max' })), 'Claude Code: Max');
			assert.strictEqual(cliAccountLabel(cli({ cli: 'codex', plan: undefined })), 'Codex');
		});

		test('a key account is named by vendor and the label chosen for it', () => {
			assert.strictEqual(providerLabel('anthropic'), 'Anthropic');
			assert.strictEqual(providerLabel('openai'), 'OpenAI');
			assert.strictEqual(providerLabel('unknown-vendor'), 'unknown-vendor');
			assert.strictEqual(keyAccountLabel(keyAccount({ provider: 'openai', label: 'Backup' })), 'OpenAI: Backup');
		});

		test('a limit window is named in words, and an unknown one keeps its raw name', () => {
			assert.strictEqual(windowLabel('five_hour'), '5-hour limit');
			assert.strictEqual(windowLabel('seven_day'), '7-day limit');
			assert.strictEqual(windowLabel('primary'), 'Primary limit');
			assert.strictEqual(windowLabel('a_new_window'), 'a_new_window');
		});
	});

	suite('matchUsageAccount', () => {

		test('matches a CLI by kind, a key account by id, and falls back to the raw id', () => {
			const clis = [cli({ cli: 'claude' })];
			const keys = [keyAccount({ id: 'key-1' })];
			assert.deepStrictEqual(matchUsageAccount(usage({ accountId: 'claude' }), clis, keys), { kind: 'cli', cli: clis[0] });
			assert.deepStrictEqual(matchUsageAccount(usage({ accountId: 'key-1' }), clis, keys), { kind: 'key', account: keys[0] });
			assert.deepStrictEqual(matchUsageAccount(usage({ accountId: 'ghost' }), clis, keys), { kind: 'unknown', accountId: 'ghost' });
		});
	});

	suite('formatting', () => {

		test('tokens sum every kind and format with grouping', () => {
			assert.strictEqual(formatTokenTotal({ inputTokens: 1000, outputTokens: 234, cacheReadTokens: 10, cacheWriteTokens: 5 }), (1249).toLocaleString());
		});

		test('cost is absent when the vendor never reports one, and formatted as dollars otherwise', () => {
			assert.strictEqual(formatCost(undefined), 'Not reported');
			assert.strictEqual(formatCost(null), 'Not reported');
			assert.strictEqual(formatCost(420000), '$0.42');
			assert.strictEqual(formatCost(0), '$0.00');
		});

		test('used percent and reset time are absent when the vendor never reports them', () => {
			assert.strictEqual(formatUsedPercent(undefined), 'Not reported');
			assert.strictEqual(formatUsedPercent(null), 'Not reported');
			assert.strictEqual(formatUsedPercent(37.6), '38% used');
			assert.strictEqual(formatResetTime(undefined), 'Not reported');
			assert.strictEqual(formatResetTime(null), 'Not reported');
			assert.strictEqual(formatResetTime('not a date'), 'Not reported');
			assert.ok(formatResetTime(new Date(Date.now() + 3600_000).toISOString()).startsWith('Resets in'));
		});

		test('a usage period renders as "tokens · cost", joined by a single middle dot (U+00B7)', () => {
			const rendered = formatPeriodValue({ inputTokens: 1000, outputTokens: 234, cacheReadTokens: 10, cacheWriteTokens: 5, costUsdMicros: 420000 });
			assert.strictEqual(rendered, `${(1249).toLocaleString()} tokens · $0.42`);
			// Guards against a mis-encoded separator (for example an extra character before it)
			// slipping back in: exactly one middle dot, at U+00B7, with plain spaces around it.
			const middleDot = rendered.indexOf('·');
			assert.notStrictEqual(middleDot, -1);
			assert.strictEqual(rendered.codePointAt(middleDot), 0xb7);
			assert.strictEqual(rendered[middleDot - 1], ' ');
			assert.strictEqual(rendered[middleDot + 1], ' ');
			assert.strictEqual(rendered.indexOf('·', middleDot + 1), -1, 'exactly one middle dot');
		});

		test('a limit window renders as "used · reset", with "Not reported" on either side when absent', () => {
			assert.strictEqual(formatLimitDetail({ usedPercent: 37.6, resetsAt: null }), '38% used · Not reported');
			assert.strictEqual(formatLimitDetail({ usedPercent: undefined, resetsAt: undefined }), 'Not reported · Not reported');
		});
	});

	suite('resolveAccountLabel', () => {

		test('prefers the remembered choice when it is still usable', () => {
			const clis = [cli({ cli: 'claude', signedIn: true, plan: 'Max' }), cli({ cli: 'codex', signedIn: true, plan: undefined })];
			const keys = [keyAccount({ id: 'key-1' })];
			assert.strictEqual(resolveAccountLabel({ kind: 'cli', cli: 'codex' }, clis, keys), 'Codex');
			assert.strictEqual(resolveAccountLabel({ kind: 'key', id: 'key-1' }, clis, keys), 'Anthropic: Prod');
		});

		test('falls back to the first signed-in CLI, then the first key account, then "No account"', () => {
			const clis = [cli({ cli: 'claude', signedIn: false }), cli({ cli: 'codex', signedIn: true, plan: 'Free' })];
			const keys = [keyAccount({ id: 'key-1' })];
			assert.strictEqual(resolveAccountLabel(undefined, clis, keys), 'Codex: Free');
			assert.strictEqual(resolveAccountLabel(undefined, [cli({ signedIn: false })], keys), 'Anthropic: Prod');
			assert.strictEqual(resolveAccountLabel(undefined, [], []), 'No account');
		});

		test('a stale choice (removed key, signed-out CLI) falls back rather than naming a ghost', () => {
			assert.strictEqual(resolveAccountLabel({ kind: 'key', id: 'gone' }, [], []), 'No account');
			assert.strictEqual(resolveAccountLabel({ kind: 'cli', cli: 'claude' }, [cli({ cli: 'claude', signedIn: false })], []), 'No account');
		});
	});

	suite('WispAccountsService', () => {

		function service(disposables: DisposableStore): { service: WispAccountsService; wispd: TestWispdService; storage: InMemoryStorageService } {
			const wispd = disposables.add(new TestWispdService());
			const storage = disposables.add(new InMemoryStorageService());
			const instance = disposables.add(new WispAccountsService(wispd, new NullLogService(), storage));
			return { service: instance, wispd, storage };
		}

		test('reload answers idle until connected, then loads keys and usage, and CLIs only when supported', async () => {
			const disposables = new DisposableStore();
			try {
				const { service: accounts, wispd } = service(disposables);
				accounts.reload();
				assert.strictEqual(accounts.clisState.get().kind, 'idle');
				assert.strictEqual(accounts.keyAccountsState.get().kind, 'idle');

				const clis = [cli()];
				const keys = [keyAccount()];
				const usageList = [usage()];
				wispd.handler = async method => {
					switch (method) {
						case 'accounts/list': return { clis, checkedAt: new Date().toISOString() };
						case 'accounts/keys/list': return { accounts: keys };
						case 'usage/get': return { accounts: usageList };
					}
					throw new Error(`unexpected method ${method}`);
				};
				wispd.setState(connected());
				await settle();

				assert.strictEqual(accounts.clisSupported.get(), false, 'capabilities: {} does not report agentClis');
				assert.strictEqual(accounts.clisState.get().kind, 'idle');
				assert.deepStrictEqual(accounts.clis.get(), []);
				assert.strictEqual(accounts.keyAccountsState.get().kind, 'ready');
				assert.deepStrictEqual(accounts.keyAccounts.get(), keys);
				assert.strictEqual(accounts.usageState.get().kind, 'ready');
				assert.deepStrictEqual(accounts.usage.get(), usageList);
			} finally {
				disposables.dispose();
			}
		});

		test('lists detected CLIs once wispd reports the agentClis capability', async () => {
			const disposables = new DisposableStore();
			try {
				const { service: accounts, wispd } = service(disposables);
				const clis = [cli()];
				wispd.handler = async method => {
					switch (method) {
						case 'accounts/list': return { clis, checkedAt: new Date().toISOString() };
						case 'accounts/keys/list': return { accounts: [] };
						case 'usage/get': return { accounts: [] };
					}
					throw new Error(`unexpected method ${method}`);
				};
				accounts.reload();
				wispd.setState(connectedWith({ agentClis: {} }));
				await settle();

				assert.strictEqual(accounts.clisSupported.get(), true);
				assert.strictEqual(accounts.clisState.get().kind, 'ready');
				assert.deepStrictEqual(accounts.clis.get(), clis);
			} finally {
				disposables.dispose();
			}
		});

		test('a failed list reports its message, and refreshClis re-probes', async () => {
			const disposables = new DisposableStore();
			try {
				const { service: accounts, wispd } = service(disposables);
				wispd.handler = async method => {
					if (method === 'accounts/keys/list') { throw new Error('store unavailable'); }
					if (method === 'usage/get') { return { accounts: [] }; }
					if (method === 'accounts/list' || method === 'accounts/refresh') { return { clis: [cli()], checkedAt: new Date().toISOString() }; }
					throw new Error(`unexpected method ${method}`);
				};
				accounts.reload();
				wispd.setState(connectedWith({ agentClis: {} }));
				await settle();

				assert.strictEqual(accounts.keyAccountsState.get().kind, 'failed');
				assert.strictEqual((accounts.keyAccountsState.get() as { message: string }).message, 'store unavailable');

				await accounts.refreshClis();
				assert.strictEqual(accounts.clisState.get().kind, 'ready');
				assert.strictEqual(accounts.clis.get().length, 1);
			} finally {
				disposables.dispose();
			}
		});

		test('a reconnect to the same wanted view reloads automatically', async () => {
			const disposables = new DisposableStore();
			try {
				const { service: accounts, wispd } = service(disposables);
				let calls = 0;
				wispd.handler = async method => {
					if (method === 'accounts/keys/list') { calls++; return { accounts: [] }; }
					if (method === 'usage/get') { return { accounts: [] }; }
					throw new Error(`unexpected method ${method}`);
				};
				accounts.reload();
				wispd.setState(connected());
				await settle();
				assert.strictEqual(calls, 1);

				wispd.setState(disconnected('exited'));
				wispd.setState(connected());
				await settle();
				assert.strictEqual(calls, 2);
			} finally {
				disposables.dispose();
			}
		});

		test('addKey and removeKey update the list from wispd\'s answer', async () => {
			const disposables = new DisposableStore();
			try {
				const { service: accounts, wispd } = service(disposables);
				const added = keyAccount({ id: 'new-id' });
				wispd.handler = async method => {
					if (method === 'accounts/keys/add') { return { account: added }; }
					if (method === 'accounts/keys/remove') { return {}; }
					throw new Error(`unexpected method ${method}`);
				};
				const result = await accounts.addKey({ id: 'new-id', provider: 'anthropic', label: 'Prod', key: 'sk-ant-secret' });
				assert.deepStrictEqual(result, added);
				assert.deepStrictEqual(accounts.keyAccounts.get(), [added]);

				await accounts.removeKey('new-id');
				assert.deepStrictEqual(accounts.keyAccounts.get(), []);
			} finally {
				disposables.dispose();
			}
		});

		test('the coordinator\'s account choice persists across instances, and clears', () => {
			const disposables = new DisposableStore();
			try {
				const wispd = disposables.add(new TestWispdService());
				const storage = disposables.add(new InMemoryStorageService());
				const first = disposables.add(new WispAccountsService(wispd, new NullLogService(), storage));
				assert.strictEqual(first.coordinatorChoice.get(), undefined);
				first.setCoordinatorChoice({ kind: 'cli', cli: 'claude' });
				assert.deepStrictEqual(first.coordinatorChoice.get(), { kind: 'cli', cli: 'claude' });

				const second = disposables.add(new WispAccountsService(disposables.add(new TestWispdService()), new NullLogService(), storage));
				assert.deepStrictEqual(second.coordinatorChoice.get(), { kind: 'cli', cli: 'claude' });

				second.setCoordinatorChoice(undefined);
				assert.strictEqual(storage.get('wisp.accounts.coordinatorChoice', StorageScope.APPLICATION), undefined);
			} finally {
				disposables.dispose();
			}
		});
	});
});
