/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { PassThrough } from 'stream';
import { join } from '../../../../base/common/path.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { IWispdTransportClose } from '../../common/wispdClient.js';
import { WispdLineReader, WispdProcessTransportFactory } from '../../node/wispdTransport.js';
import { resolveWispdExecutable } from '../../node/wispdService.js';

suite('WispdLineReader', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	function read(maxFrameBytes: number) {
		const stream = new PassThrough();
		const reader = store.add(new WispdLineReader(stream, maxFrameBytes));
		const lines: string[] = [];
		const overflows: number[] = [];
		let data = 0;
		store.add(reader.onLine(line => lines.push(line)));
		store.add(reader.onOverflow(length => overflows.push(length)));
		store.add(reader.onData(() => data++));
		const write = (chunk: string) => stream.write(Buffer.from(chunk, 'utf8'));
		return { lines, overflows, data: () => data, write, stream };
	}

	async function flush(): Promise<void> {
		await new Promise(resolve => setImmediate(resolve));
	}

	test('splits lines across and within chunks', async () => {
		const reader = read(1024);
		reader.write('{"a":');
		reader.write('1}\n{"b":2}\n{"c"');
		reader.write(':3}\r\n');
		await flush();
		assert.deepStrictEqual(reader.lines, ['{"a":1}', '{"b":2}', '{"c":3}']);
		assert.strictEqual(reader.data(), 3, 'every chunk counts as data, even without a whole line');
	});

	test('keeps multi-byte characters split across chunks', async () => {
		const reader = read(1024);
		const bytes = Buffer.from('"é"\n', 'utf8');
		reader.stream.write(bytes.subarray(0, 2));
		reader.stream.write(bytes.subarray(2));
		await flush();
		assert.deepStrictEqual(reader.lines, ['"é"']);
	});

	test('accepts a line of exactly maxFrameBytes', async () => {
		const reader = read(8);
		reader.write('12345678\n');
		await flush();
		assert.deepStrictEqual(reader.lines, ['12345678']);
		assert.deepStrictEqual(reader.overflows, []);
	});

	test('reports a line longer than maxFrameBytes without buffering it', async () => {
		const reader = read(8);
		reader.write('1234');
		reader.write('56789');
		reader.write('\n{}\n');
		await flush();
		assert.deepStrictEqual(reader.lines, []);
		assert.deepStrictEqual(reader.overflows, [9]);
	});

	test('reports an oversized line inside one chunk', async () => {
		const reader = read(8);
		reader.write('{}\n123456789\n');
		await flush();
		assert.deepStrictEqual(reader.overflows, [9]);
	});
});

suite('WispdProcessTransport', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	function run(script: string): Promise<{ lines: string[]; close: IWispdTransportClose }> {
		const factory = new WispdProcessTransportFactory({ executable: process.execPath, args: ['-e', script], env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }, maxFrameBytes: 64 }, new NullLogger());
		const transport = store.add(factory.create());
		const lines: string[] = [];
		store.add(transport.onDidReceiveLine(line => lines.push(line)));
		return new Promise(resolve => store.add(transport.onDidClose(close => resolve({ lines, close }))));
	}

	test('reads lines, and reports exit code 4 as unreachable with stderr', async () => {
		const { lines, close } = await run(`process.stdout.write('{"x":1}\\n'); process.stderr.write('wispd attach: could not start wispd\\n'); process.exitCode = 4;`);
		assert.deepStrictEqual(lines, ['{"x":1}']);
		assert.deepStrictEqual(close, { reason: 'unreachable', message: 'wispd attach could not reach or start wispd.', exitCode: 4, stderr: 'wispd attach: could not start wispd' });
	});

	test('closes on a frame over maxFrameBytes', async () => {
		const { close } = await run(`process.stdout.write('x'.repeat(100)); setTimeout(() => {}, 10000);`);
		assert.strictEqual(close.reason, 'frameTooLarge');
	});

	test('reports a missing binary', async () => {
		const factory = new WispdProcessTransportFactory({ executable: '/nonexistent/wispd', args: ['attach'] }, new NullLogger());
		const transport = store.add(factory.create());
		const close = await new Promise<IWispdTransportClose>(resolve => store.add(transport.onDidClose(resolve)));
		assert.strictEqual(close.reason, 'spawnFailed');
		assert.match(close.message, /was not found/);
	});
});

suite('resolveWispdExecutable', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	const appRoot = join('/', 'repo', 'editor', 'vscode');
	const bundled = join(appRoot, 'bin', 'wispd');
	const repoBuild = join('/', 'repo', 'target', 'debug', 'wispd');

	test('prefers the environment override', () => {
		assert.strictEqual(resolveWispdExecutable({ WISP_WISPD_PATH: '/custom/wispd' }, appRoot, true, () => true), '/custom/wispd');
	});

	test('then the bundled binary', () => {
		assert.strictEqual(resolveWispdExecutable({}, appRoot, false, path => path === bundled || path === repoBuild), bundled);
	});

	test('then the repo build, only in development', () => {
		assert.strictEqual(resolveWispdExecutable({}, appRoot, false, path => path === repoBuild), repoBuild);
		assert.strictEqual(resolveWispdExecutable({}, appRoot, true, path => path === repoBuild), 'wispd');
	});

	test('then PATH', () => {
		assert.strictEqual(resolveWispdExecutable({}, appRoot, false, () => false), 'wispd');
	});
});
