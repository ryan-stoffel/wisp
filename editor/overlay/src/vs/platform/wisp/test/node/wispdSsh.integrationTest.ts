/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { existsSync, promises as fs } from 'fs';
import { tmpdir } from 'os';
import { join } from '../../../../base/common/path.js';
import { Event } from '../../../../base/common/event.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { NullLogger } from '../../../log/common/log.js';
import { WispdState } from '../../common/wispd.js';
import { WispdClient } from '../../common/wispdClient.js';
import { WispdSshTransportFactory } from '../../node/wispdSshTransport.js';

/**
 * Drives the real `WispdSshTransportFactory` (so real argv parsing, real PATH lookup of `ssh`)
 * against a fake `ssh` on `PATH`, since real `ssh localhost` isn't available on Ryan's Mac (#95
 * covers a real one in CI). The fake mimics ssh's exit codes and stderr for each error class from
 * decision record 0007, and for a successful handshake execs a real `wispd attach`: set
 * WISP_TEST_WISPD to a binary from `cargo build -p wispd`. scripts/editor/test-wisp does.
 */
const wispdPath = process.env.WISP_TEST_WISPD;

const FAKE_SSH = `#!/usr/bin/env node
'use strict';
const cp = require('child_process');
const args = process.argv.slice(2);
const dashIndex = args.indexOf('--');
const rest = args.slice(dashIndex + 1);
const remoteCommand = rest[1];
const mode = process.env.FAKE_SSH_MODE || 'success';

function fail(code, stderr) {
	if (stderr) {
		process.stderr.write(stderr + '\\n');
	}
	process.exit(code);
}

switch (mode) {
	case 'auth-failed':
		fail(255, 'Permission denied (publickey,password,keyboard-interactive).');
		break;
	case 'host-key':
		fail(255, 'Host key verification failed.');
		break;
	case 'host-key-changed':
		fail(255, '@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\\n@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\\nHost key verification failed.');
		break;
	case 'no-route':
		fail(255, 'ssh: connect to host fake port 22: Operation timed out');
		break;
	case 'wispd-missing':
		fail(127, 'bash: line 1: ' + remoteCommand + ': command not found');
		break;
	case 'path-fallback':
		if (remoteCommand !== process.env.FAKE_SSH_GOOD_REMOTE) {
			fail(127, 'bash: line 1: ' + remoteCommand + ': command not found');
			break;
		}
	// falls through
	case 'noisy-path-fallback':
		if (remoteCommand !== process.env.FAKE_SSH_GOOD_REMOTE) {
			process.stdout.write('Last login: Tue Sep 24 on ttys000\\n');
			fail(127, 'bash: line 1: ' + remoteCommand + ': command not found');
			break;
		}
	// falls through
	case 'success': {
		const result = cp.spawnSync(process.env.FAKE_SSH_WISPD, ['attach'], { stdio: 'inherit', env: process.env });
		process.exit(result.status === null ? 1 : result.status);
		break;
	}
	default:
		fail(2, 'fake-ssh: unknown FAKE_SSH_MODE ' + mode);
}
`;

function within<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
	let handle: ReturnType<typeof setTimeout>;
	const timeout = new Promise<never>((_, reject) => handle = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms));
	return Promise.race([promise, timeout]).finally(() => clearTimeout(handle));
}

function whenState<K extends WispdState['kind']>(client: WispdClient, kind: K): Promise<Extract<WispdState, { kind: K }>> {
	if (client.state.kind === kind) {
		return Promise.resolve(client.state as Extract<WispdState, { kind: K }>);
	}
	return within(Event.toPromise(Event.filter(client.onDidChangeState, state => state.kind === kind)) as unknown as Promise<Extract<WispdState, { kind: K }>>, 20_000, `state ${kind}`);
}

suite('wispd over ssh (integration)', function () {

	this.timeout(60_000);

	let binDir: string;
	let dataDir: string;
	let store: DisposableStore;

	setup(async function () {
		if (!wispdPath) {
			this.skip();
		}
		assert.ok(existsSync(wispdPath), `WISP_TEST_WISPD is ${wispdPath}, which does not exist`);
		binDir = await fs.mkdtemp(join(tmpdir(), 'wispd-fake-ssh-'));
		await fs.writeFile(join(binDir, 'ssh'), FAKE_SSH, { mode: 0o755 });
		dataDir = await fs.mkdtemp(join(tmpdir(), 'wispd-ssh-data-'));
		store = new DisposableStore();
	});

	teardown(async () => {
		store?.dispose();
		if (dataDir) {
			await stopDaemon(dataDir);
			await fs.rm(dataDir, { recursive: true, force: true });
		}
		if (binDir) {
			await fs.rm(binDir, { recursive: true, force: true });
		}
	});

	function client(mode: string, extraEnv: NodeJS.ProcessEnv = {}, candidates: readonly string[] = ['wispd']): WispdClient {
		const env: NodeJS.ProcessEnv = {
			...process.env,
			PATH: `${binDir}:${process.env.PATH}`,
			FAKE_SSH_MODE: mode,
			FAKE_SSH_WISPD: wispdPath!,
			WISPD_DATA_DIR: dataDir,
			...extraEnv,
		};
		const factory = new WispdSshTransportFactory({ destination: 'fake-host', remoteWispdCandidates: candidates, env }, new NullLogger());
		return store.add(new WispdClient(factory, { client: { name: 'wisp-ssh-test', version: '0.0.0' } }, new NullLogger()));
	}

	test('a successful handshake over the fake ssh', async () => {
		const c = client('success');
		c.start();
		const connected = await whenState(c, 'connected');
		assert.strictEqual(connected.protocol, 1);
	});

	test('falls back from PATH to the configured Homebrew path on exit 127', async () => {
		const c = client('path-fallback', { FAKE_SSH_GOOD_REMOTE: '/opt/homebrew/bin/wispd' }, ['wispd', '/opt/homebrew/bin/wispd', '/usr/local/bin/wispd']);
		c.start();
		const connected = await whenState(c, 'connected');
		assert.strictEqual(connected.protocol, 1);
	});

	test('B1: still falls back and completes the handshake when the first candidate prints a noisy line before exit 127', async () => {
		const c = client('noisy-path-fallback', { FAKE_SSH_GOOD_REMOTE: '/opt/homebrew/bin/wispd' }, ['wispd', '/opt/homebrew/bin/wispd', '/usr/local/bin/wispd']);
		c.start();
		const connected = await whenState(c, 'connected');
		assert.strictEqual(connected.protocol, 1);
	});

	test('reports wispdNotFound once every candidate is exhausted', async () => {
		const c = client('wispd-missing', {}, ['wispd', '/opt/homebrew/bin/wispd']);
		c.start();
		const disconnected = await whenState(c, 'disconnected');
		assert.strictEqual(disconnected.reason, 'wispdNotFound');
	});

	test('maps an auth failure', async () => {
		const c = client('auth-failed');
		c.start();
		const disconnected = await whenState(c, 'disconnected');
		assert.strictEqual(disconnected.reason, 'authFailed');
	});

	test('maps an unknown host key', async () => {
		const c = client('host-key');
		c.start();
		const disconnected = await whenState(c, 'disconnected');
		assert.strictEqual(disconnected.reason, 'hostKeyUnknown');
	});

	test('maps a changed host key as hostKeyChanged, distinct from an unknown one', async () => {
		const c = client('host-key-changed');
		c.start();
		const disconnected = await whenState(c, 'disconnected');
		assert.strictEqual(disconnected.reason, 'hostKeyChanged');
		assert.match(disconnected.message, /possible attack/);
	});

	test('maps no route to host', async () => {
		const c = client('no-route');
		c.start();
		const disconnected = await whenState(c, 'disconnected');
		assert.strictEqual(disconnected.reason, 'noRoute');
	});
});

/** Stops the `serve` a test's `attach` started, through its lock file (decision record 0009). */
async function stopDaemon(dataDir: string): Promise<void> {
	const lock = join(dataDir, 'wispd.lock');
	let pid: number;
	try {
		pid = Number((await fs.readFile(lock, 'utf8')).trim());
	} catch {
		return;
	}
	if (!pid) {
		return;
	}
	try {
		process.kill(pid, 'SIGTERM');
	} catch {
		return;
	}
	for (let i = 0; i < 200; i++) {
		try {
			process.kill(pid, 0);
		} catch {
			return;
		}
		await new Promise(resolve => setTimeout(resolve, 50));
	}
}
