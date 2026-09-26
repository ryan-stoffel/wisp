/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { encodeBase64, VSBuffer } from '../../../../base/common/buffer.js';
import { Event } from '../../../../base/common/event.js';
import { URI } from '../../../../base/common/uri.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { FileSystemProviderError, FileSystemProviderErrorCode } from '../../../files/common/files.js';
import { agentFileUri, agentReviewItems, agentReviewUri, parseAgentFileUri, parseAgentReviewUri, WispAgentFileSystemProvider } from '../../common/wispAgentFiles.js';
import { IWispdService, WispdError, WispdMethod } from '../../common/wispd.js';
import type { AgentDiffResult, AgentFileParams, AgentFileResult, WispRequests } from '../../common/wispProtocol.js';

const RUN = '01a0d360-1a2b-7c3d-8e4f-5a6b7c8d9e01';
const BASE = '4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b';
const HEAD = '9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c';

/** A wispd that answers `agent/file` from a table, and records what it was asked. */
class FakeWispd implements IWispdService {
	declare readonly _serviceBrand: undefined;
	readonly onDidChangeState = Event.None;
	readonly asked: AgentFileParams[] = [];

	constructor(private readonly answer: (params: AgentFileParams) => AgentFileResult) { }

	getState(): never {
		throw new Error('not used');
	}

	async retry(): Promise<void> { }

	async request<M extends WispdMethod>(method: M, params: WispRequests[M]['params']): Promise<WispRequests[M]['result']> {
		assert.strictEqual(method, 'agent/file');
		this.asked.push(params as AgentFileParams);
		return this.answer(params as AgentFileParams) as WispRequests[M]['result'];
	}

	subscribe(): never {
		throw new Error('not used');
	}
}

function file(params: AgentFileParams, content: string | undefined, commit = params.side === 'base' ? BASE : HEAD): AgentFileResult {
	if (content === undefined) {
		return { path: params.path, side: params.side, commit, exists: false, tooLarge: false };
	}
	const bytes = VSBuffer.fromString(content);
	return { path: params.path, side: params.side, commit, exists: true, size: bytes.byteLength, content: encodeBase64(bytes), tooLarge: false };
}

async function code(promise: Promise<unknown>): Promise<string> {
	try {
		await promise;
	} catch (error) {
		assert.ok(error instanceof FileSystemProviderError, String(error));
		return error.code;
	}
	assert.fail('expected an error');
}

suite('wispAgentFiles', () => {

	const store = ensureNoDisposablesAreLeakedInTestSuite();

	test('file and review URIs round-trip, including awkward paths', () => {
		for (const path of ['README.md', 'src/a b/c#d?.ts', 'dir/ünïcode.md']) {
			const uri = agentFileUri({ runId: RUN, side: 'head', path, commit: HEAD });
			const reparsed = parseAgentFileUri(URI.parse(uri.toString()));
			assert.deepStrictEqual(reparsed, { runId: RUN, side: 'head', path, commit: HEAD });
		}
		assert.strictEqual(parseAgentFileUri(URI.parse('wisp-agent://run/middle/README.md')), undefined);
		assert.strictEqual(parseAgentFileUri(URI.file('/etc/passwd')), undefined);
		assert.deepStrictEqual(parseAgentReviewUri(agentReviewUri(RUN, HEAD)), { runId: RUN, head: HEAD });
	});

	test('review items give added files no base side and deleted files no head side', () => {
		const diff: AgentDiffResult = {
			base: BASE,
			head: HEAD,
			stats: { files: 4, insertions: 3, deletions: 2 },
			truncated: false,
			files: [
				{ path: 'added.md', status: 'added', insertions: 1, deletions: 0, binary: false, diffTruncated: false },
				{ path: 'gone.md', status: 'deleted', insertions: 0, deletions: 1, binary: false, diffTruncated: false },
				{ path: 'new.ts', oldPath: 'old.ts', status: 'renamed', insertions: 1, deletions: 1, binary: false, diffTruncated: false },
				{ path: 'same.ts', status: 'modified', insertions: 1, deletions: 0, binary: false, diffTruncated: false },
			],
		};
		const items = agentReviewItems(RUN, diff).map(item => [item.original && parseAgentFileUri(item.original), item.modified && parseAgentFileUri(item.modified)]);
		assert.deepStrictEqual(items, [
			[undefined, { runId: RUN, side: 'head', path: 'added.md', commit: HEAD }],
			[{ runId: RUN, side: 'base', path: 'gone.md', commit: BASE }, undefined],
			[{ runId: RUN, side: 'base', path: 'old.ts', commit: BASE }, { runId: RUN, side: 'head', path: 'new.ts', commit: HEAD }],
			[{ runId: RUN, side: 'base', path: 'same.ts', commit: BASE }, { runId: RUN, side: 'head', path: 'same.ts', commit: HEAD }],
		]);
	});

	test('reads both sides through agent/file, exactly', async () => {
		const wispd = new FakeWispd(params => file(params, params.side === 'base' ? 'hello\n' : 'hello\r\nworld ✓'));
		const provider = store.add(new WispAgentFileSystemProvider(wispd));
		const head = await provider.readFile(agentFileUri({ runId: RUN, side: 'head', path: 'README.md', commit: HEAD }));
		assert.strictEqual(VSBuffer.wrap(head).toString(), 'hello\r\nworld ✓');
		const base = await provider.readFile(agentFileUri({ runId: RUN, side: 'base', path: 'README.md', commit: BASE }));
		assert.strictEqual(VSBuffer.wrap(base).toString(), 'hello\n');
		const stat = await provider.stat(agentFileUri({ runId: RUN, side: 'base', path: 'README.md', commit: BASE }));
		assert.strictEqual(stat.size, 6);
		assert.deepStrictEqual(wispd.asked[0], { runId: RUN, path: 'README.md', side: 'head' });
	});

	test('a missing, too large, outdated, or accepted file is an error, never empty content', async () => {
		const provider = store.add(new WispAgentFileSystemProvider(new FakeWispd(params => {
			switch (params.path) {
				case 'missing.md': return file(params, undefined);
				case 'big.bin': return { path: params.path, side: params.side, commit: HEAD, exists: true, size: 50_000_000, tooLarge: true };
				case 'moved-on.md': return file(params, 'newer', 'ffffffffffffffffffffffffffffffffffffffff');
				default: throw new WispdError(-32000, 'run was accepted', 'runAccepted');
			}
		})));
		const uri = (path: string) => agentFileUri({ runId: RUN, side: 'head', path, commit: HEAD });
		assert.strictEqual(await code(provider.readFile(uri('missing.md'))), FileSystemProviderErrorCode.FileNotFound);
		assert.strictEqual(await code(provider.readFile(uri('big.bin'))), FileSystemProviderErrorCode.FileTooLarge);
		assert.strictEqual(await code(provider.readFile(uri('moved-on.md'))), FileSystemProviderErrorCode.Unavailable);
		assert.strictEqual(await code(provider.readFile(uri('accepted.md'))), FileSystemProviderErrorCode.FileNotFound);
		assert.strictEqual(await code(provider.readFile(URI.file('/etc/passwd'))), FileSystemProviderErrorCode.FileNotFound);
	});

	test('is read-only', async () => {
		const provider = store.add(new WispAgentFileSystemProvider(new FakeWispd(params => file(params, 'x'))));
		const uri = agentFileUri({ runId: RUN, side: 'head', path: 'README.md', commit: HEAD });
		assert.strictEqual(await code(provider.writeFile(uri, new Uint8Array(), { create: true, overwrite: true, unlock: false, atomic: false })), FileSystemProviderErrorCode.NoPermissions);
		assert.strictEqual(await code(provider.delete(uri, { recursive: false, useTrash: false, atomic: false })), FileSystemProviderErrorCode.NoPermissions);
		assert.strictEqual(await code(provider.rename(uri, uri, { overwrite: true })), FileSystemProviderErrorCode.NoPermissions);
	});
});
