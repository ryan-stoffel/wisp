/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { VSBuffer } from '../../../../../base/common/buffer.js';
import { DisposableStore, toDisposable } from '../../../../../base/common/lifecycle.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { URI } from '../../../../../base/common/uri.js';
import { FileChangeType, FileSystemProviderErrorCode, FileType } from '../../../../../platform/files/common/files.js';
import { IQuickInputService } from '../../../../../platform/quickinput/common/quickInput.js';
import { WispdError } from '../../../../../platform/wisp/common/wispd.js';
import type { ContextFile } from '../../../../../platform/wisp/common/wispProtocol.js';
import { IEditorService } from '../../../../../workbench/services/editor/common/editorService.js';
import { validateContextFileName, WispContextAddFileFlow } from '../../browser/wispContextAddFile.js';
import { WispContextFileSystemProvider } from '../../browser/wispContextFileSystemProvider.js';
import { IWispContextService, WispContextService } from '../../browser/wispContextService.js';
import { contextFileDetail, projectFacts } from '../../browser/wispProjectView.js';
import { parseContextUri, toContextUri } from '../../common/wispContextUri.js';
import { agentsWindowServices, IAgentsWindowServices, settle } from './wispAgentsTestServices.js';
import { connected, connecting, disconnected, project, SSH_COMMAND } from './wispHostTestUtils.js';

const ONE = '0192f0c4-0000-7000-8000-000000000001';
const TWO = '0192f0c4-0000-7000-8000-000000000002';

function contextFile(path: string, options: Partial<ContextFile> = {}): ContextFile {
	return { path, size: 5, modifiedAt: '2026-09-24T12:00:00Z', lastWriter: 'editor', ...options };
}

suite('wisp: shared context', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	/** Services whose wispd answers `project/list` at `seq` 9 and `context/list` with `files`. */
	function services(host = 'local', files: ContextFile[] = [contextFile('notes.md')]): IAgentsWindowServices & { contextService: WispContextService } {
		const context = agentsWindowServices(disposables, true, host);
		// Every answer copies its files: real IPC never hands the caller wispd's own objects, and an
		// alias here would let `upsert`'s change check see a write that already mutated its baseline.
		context.wispd.handler = async (method, params) => {
			switch (method) {
				case 'project/list':
					return { projects: [], seq: 9 };
				case 'context/list':
					return { files: files.map(file => ({ ...file })) };
				case 'context/read': {
					const { path } = params as { path: string };
					const file = files.find(candidate => candidate.path === path);
					if (!file) {
						throw new WispdError(-32000, `no shared context file named ${path}`, 'contextNotFound');
					}
					return { file: { ...file }, content: `# ${path}` };
				}
				case 'context/write': {
					const { path, content, writer } = params as { path: string; content: string; writer?: string };
					const file: ContextFile = { path, size: content.length, modifiedAt: '2026-09-25T00:00:00Z', lastWriter: writer };
					const index = files.findIndex(candidate => candidate.path === path);
					if (index === -1) {
						files.push(file);
					} else {
						files[index] = file;
					}
					return { file: { ...file } };
				}
			}
			throw new Error(`unexpected ${method}`);
		};
		const contextService = disposables.add(context.instantiationService.createInstance(WispContextService));
		context.instantiationService.stub(IWispContextService, contextService);
		return { ...context, contextService };
	}

	function readyFiles(contextService: WispContextService, id: string): readonly ContextFile[] {
		const state = contextService.state(id).get();
		return state.kind === 'ready' ? state.files : [];
	}

	/**
	 * The active subscription scoped to `project`, distinct from `WispProjectsService`'s own
	 * host-level one, which `agentsWindowServices` always creates alongside it.
	 */
	function contextSubscription(wispd: IAgentsWindowServices['wispd'], id = ONE) {
		const found = [...wispd.subscriptions].reverse().find(candidate => candidate.active && candidate.options.project === id);
		if (!found) {
			throw new Error(`no active subscription for project ${id}`);
		}
		return found;
	}

	suite('service', () => {

		test('lists once watched and connected, subscribing from project/list\'s seq, scoped to the project', async () => {
			const { wispd, contextService } = services();
			const state = contextService.state(ONE);
			assert.strictEqual(state.get().kind, 'idle');
			assert.deepStrictEqual(wispd.requests, [], 'nothing is asked before the host connects');

			wispd.setState(connected());
			await settle();
			assert.strictEqual(state.get().kind, 'ready');
			assert.deepStrictEqual(readyFiles(contextService, ONE).map(f => f.path), ['notes.md']);
			assert.deepStrictEqual(contextSubscription(wispd).options, { after: 9, project: ONE, logId: 'log-1' });
		});

		test('a project asked for after the host is already connected lists it right away', async () => {
			const { wispd, contextService } = services();
			wispd.setState(connected());
			await settle();
			contextService.state(TWO);
			await settle();
			assert.strictEqual(contextService.state(TWO).get().kind, 'ready');
		});

		test('context.changed upserts by path, sorted, and unknown kinds are skipped', async () => {
			const { wispd, contextService } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			const emitter = contextSubscription(wispd).emitter;
			const event = (file: ContextFile, seq: number) => ({ type: 'event' as const, event: { seq, time: '2026-09-25T00:00:00Z', project: ONE, event: { kind: 'context.changed' as const, file } } });
			emitter.fire(event(contextFile('notes.md', { size: 99 }), 10));
			emitter.fire(event(contextFile('agenda.md'), 11));
			emitter.fire({ type: 'event', event: { seq: 12, time: '2026-09-25T00:00:00Z', project: ONE, event: { kind: 'agent.started', runId: 'r1' } as never } });
			assert.deepStrictEqual(readyFiles(contextService, ONE).map(f => [f.path, f.size]), [['agenda.md', 5], ['notes.md', 99]]);
		});

		test('a reconnect keeps the subscription, which replays; a resync lists again', async () => {
			const { wispd, contextService } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			wispd.setState(disconnected('exited'));
			wispd.setState(connecting(2));
			wispd.setState(connected());
			await settle();
			assert.strictEqual(wispd.subscriptions.filter(s => s.active && s.options.project === ONE).length, 1);
			const requestsBefore = wispd.requests.filter(([method]) => method === 'context/list').length;

			contextSubscription(wispd).emitter.fire({ type: 'resync', reason: 'logIdChanged' });
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready');
			const requestsAfter = wispd.requests.filter(([method]) => method === 'context/list').length;
			assert.strictEqual(requestsAfter, requestsBefore + 1, 'the resync re-listed once');
			assert.strictEqual(wispd.subscriptions.filter(s => s.active && s.options.project === ONE).length, 1, 'the old subscription ended');
		});

		test('another host resets a watched project to idle, then lists again once connected', async () => {
			const { wispd, contextService } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			assert.strictEqual(readyFiles(contextService, ONE).length, 1);

			wispd.setState(connected(SSH_COMMAND));
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready', 'the new host\'s list answered');
		});

		test('a failed list reports wispd\'s message, and reload tries again', async () => {
			const { wispd, contextService } = services();
			const handler = wispd.handler!;
			wispd.handler = async (method, params) => {
				if (method === 'context/list') {
					throw new WispdError(-32603, 'the shared context folder is unavailable', undefined);
				}
				return handler(method, params);
			};
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			const state = contextService.state(ONE).get();
			assert.strictEqual(state.kind, 'failed');
			assert.match(state.kind === 'failed' ? state.message : '', /unavailable/);

			wispd.handler = handler;
			contextService.reload(ONE);
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready');
		});

		test('write is attributed to the editor, and optimistically upserts a watched project', async () => {
			const { wispd, contextService } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();

			const file = await contextService.write(ONE, 'plan.md', '# Plan');
			assert.strictEqual(file.lastWriter, 'editor');
			assert.deepStrictEqual(readyFiles(contextService, ONE).map(f => f.path), ['notes.md', 'plan.md']);
			const write = wispd.requests.find(([method]) => method === 'context/write');
			assert.strictEqual((write?.[1] as { writer?: string }).writer, 'editor');
		});

		test('read is a plain passthrough that needs no watch or list', async () => {
			const { wispd, contextService } = services();
			wispd.setState(connected());
			await settle();
			const result = await contextService.read(ONE, 'notes.md');
			assert.strictEqual(result.content, '# notes.md');
			assert.ok(!wispd.requests.some(([method]) => method === 'context/list'), 'read alone does not list the shared context');
		});
	});

	suite('file system provider', () => {

		function provider(context: IAgentsWindowServices) {
			return disposables.add(context.instantiationService.createInstance(WispContextFileSystemProvider));
		}

		test('stat, readFile, and writeFile round-trip through context/read and context/write', async () => {
			const context = services();
			context.wispd.setState(connected());
			await settle();
			const fsProvider = provider(context);
			const uri = toContextUri(ONE, 'notes.md');

			const stat = await fsProvider.stat(uri);
			assert.strictEqual(stat.type, FileType.File);
			assert.strictEqual(stat.size, 5);

			const read = await fsProvider.readFile(uri);
			assert.strictEqual(VSBuffer.wrap(read).toString(), '# notes.md');

			await fsProvider.writeFile(uri, VSBuffer.fromString('updated').buffer, { create: false, overwrite: true, unlock: false, atomic: false });
			const written = context.wispd.requests.find(([method]) => method === 'context/write');
			assert.strictEqual((written?.[1] as { content: string }).content, 'updated');
		});

		test('a missing file maps to FileNotFound', async () => {
			const context = services();
			context.wispd.setState(connected());
			await settle();
			const fsProvider = provider(context);
			await assert.rejects(fsProvider.readFile(toContextUri(ONE, 'missing.md')), (error: Error) =>
				(error as unknown as { code: string }).code === FileSystemProviderErrorCode.FileNotFound);
		});

		test('the project\'s folder is a directory, and writing or reading it is refused', async () => {
			const context = services();
			const fsProvider = provider(context);
			const root = URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' });
			const stat = await fsProvider.stat(root);
			assert.strictEqual(stat.type, FileType.Directory);
			await assert.rejects(fsProvider.readFile(root), (error: Error) =>
				(error as unknown as { code: string }).code === FileSystemProviderErrorCode.FileIsADirectory);
		});

		test('mkdir, delete, and rename are refused: wispd\'s protocol has none of them', async () => {
			const context = services();
			const fsProvider = provider(context);
			const uri = toContextUri(ONE, 'notes.md');
			await assert.rejects(fsProvider.mkdir(uri));
			await assert.rejects(fsProvider.delete(uri, { recursive: false, useTrash: false, atomic: false }));
			await assert.rejects(fsProvider.rename(uri, toContextUri(ONE, 'renamed.md'), { overwrite: true }));
		});

		test('readdir lists the project\'s known files', async () => {
			const context = services('local', [contextFile('a.md'), contextFile('b.txt')]);
			context.wispd.setState(connected());
			await settle();
			// Warms the watch, as the Project tab's section would have already done.
			context.contextService.state(ONE);
			await settle();
			const fsProvider = provider(context);
			const entries = await fsProvider.readdir(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' }));
			assert.deepStrictEqual(entries, [['a.md', FileType.File], ['b.txt', FileType.File]]);
		});

		test('watch fires onDidChangeFile on a context.changed event for that path, not on the first read', async () => {
			const context = services();
			context.wispd.setState(connected());
			await settle();
			const fsProvider = provider(context);
			const uri = toContextUri(ONE, 'notes.md');
			const fired: FileChangeType[] = [];
			const watching = disposables.add(fsProvider.onDidChangeFile(changes => fired.push(...changes.map(c => c.type))));
			const stop = fsProvider.watch(uri, { recursive: false, excludes: [], includes: [] });
			await settle();
			assert.deepStrictEqual(fired, [], 'the initial value is not itself a change');

			await context.contextService.write(ONE, 'notes.md', 'changed');
			await settle();
			assert.deepStrictEqual(fired, [FileChangeType.UPDATED]);
			stop.dispose();
			watching.dispose();
		});

		test('the root and unparsable resources are not watched', () => {
			const context = services();
			const fsProvider = provider(context);
			const store = new DisposableStore();
			store.add(fsProvider.watch(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' }), { recursive: false, excludes: [], includes: [] }));
			store.add(fsProvider.watch(URI.file('/not-ours'), { recursive: false, excludes: [], includes: [] }));
			store.dispose();
		});
	});

	suite('add file', () => {

		test('validateContextFileName rejects paths, dot-files, bad extensions, empty and long names', () => {
			assert.strictEqual(validateContextFileName(''), 'Enter a file name.');
			assert.match(validateContextFileName('a/b.md') ?? '', /not a path/);
			assert.match(validateContextFileName('.hidden.md') ?? '', /dot/);
			assert.match(validateContextFileName('notes.pdf') ?? '', /\.md, \.markdown, or \.txt/);
			assert.strictEqual(validateContextFileName('notes.md'), undefined);
			assert.strictEqual(validateContextFileName('agenda.markdown'), undefined);
			assert.strictEqual(validateContextFileName('todo.txt'), undefined);
			assert.match(validateContextFileName('a'.repeat(253) + '.md') ?? '', /at most 255 bytes/);
		});

		test('run asks for a name, writes it empty and attributed to the editor, and opens it', async () => {
			const context = services();
			context.wispd.setState(connected());
			await settle();
			const opened: URI[] = [];
			context.instantiationService.stub(IEditorService, { openEditor: async (input: { resource: URI }) => { opened.push(input.resource); return undefined; } } as unknown as IEditorService);
			context.instantiationService.stub(IQuickInputService, {
				createInputBox: () => {
					const store = new DisposableStore();
					const listeners: { accept?: () => void; hide?: () => void } = {};
					const box = {
						value: '', title: '', prompt: '', placeholder: '', ignoreFocusOut: false, validationMessage: undefined as string | undefined, severity: 0,
						onDidAccept: (fn: () => void) => { listeners.accept = fn; return store.add(toDisposable(() => { listeners.accept = undefined; })); },
						onDidChangeValue: () => store.add(toDisposable(() => { /* unused in this flow */ })),
						onDidHide: (fn: () => void) => { listeners.hide = fn; return store.add(toDisposable(() => { listeners.hide = undefined; })); },
						show: () => { box.value = 'plan.md'; queueMicrotask(() => { listeners.accept?.(); listeners.hide?.(); }); },
						hide: () => listeners.hide?.(),
						dispose: () => store.dispose(),
					};
					return box;
				},
			} as unknown as IQuickInputService);

			const file = await context.instantiationService.createInstance(WispContextAddFileFlow).run(ONE);
			assert.strictEqual(file?.path, 'plan.md');
			const write = context.wispd.requests.find(([method]) => method === 'context/write');
			assert.strictEqual((write?.[1] as { content: string; writer?: string }).content, '');
			assert.strictEqual((write?.[1] as { content: string; writer?: string }).writer, 'editor');
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [toContextUri(ONE, 'plan.md').toString()]);
		});
	});

	suite('Project tab facts and pure helpers', () => {

		test('the Shared context fact reflects the file count once it loads', () => {
			assert.deepStrictEqual(projectFacts(project(ONE, 'billing'), 'this Mac', '/Users/ryan', 0).map(f => [f.label, f.detail, f.done])[2], ['Shared context', '0 files', false]);
			assert.deepStrictEqual(projectFacts(project(ONE, 'billing'), 'this Mac', '/Users/ryan', 1).map(f => [f.label, f.detail, f.done])[2], ['Shared context', '1 file', true]);
			assert.deepStrictEqual(projectFacts(project(ONE, 'billing'), 'this Mac', '/Users/ryan', 3).map(f => [f.label, f.detail, f.done])[2], ['Shared context', '3 files', true]);
			// Omitted (not yet loaded) reads the same as the existing placeholder.
			assert.deepStrictEqual(projectFacts(project(ONE, 'billing'), 'this Mac', '/Users/ryan').map(f => [f.label, f.detail, f.done])[2], ['Shared context', '0 files', false]);
		});

		test('contextFileDetail names the writer and when, or just when for a file wispd never saw written', () => {
			assert.strictEqual(contextFileDetail(contextFile('notes.md', { lastWriter: 'editor', modifiedAt: new Date(Date.now() - 60_000).toISOString() })), 'editor · 1 min ago');
			assert.match(contextFileDetail(contextFile('notes.md', { lastWriter: undefined, modifiedAt: new Date(Date.now() - 60_000).toISOString() })), /^1 min ago$/);
		});
	});

	suite('URI helpers', () => {

		test('toContextUri and parseContextUri round-trip, and reject other schemes', () => {
			const uri = toContextUri(ONE, 'notes.md');
			assert.strictEqual(uri.scheme, 'wisp-context');
			assert.strictEqual(uri.authority, ONE);
			assert.deepStrictEqual(parseContextUri(uri), { project: ONE, path: 'notes.md' });
			assert.deepStrictEqual(parseContextUri(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' })), { project: ONE, path: '' });
			assert.strictEqual(parseContextUri(URI.file('/notes.md')), undefined);
		});
	});
});
