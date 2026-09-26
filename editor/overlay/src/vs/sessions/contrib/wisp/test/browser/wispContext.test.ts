/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { VSBuffer } from '../../../../../base/common/buffer.js';
import { DisposableStore, toDisposable } from '../../../../../base/common/lifecycle.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { Emitter } from '../../../../../base/common/event.js';
import { observableValue } from '../../../../../base/common/observable.js';
import { URI } from '../../../../../base/common/uri.js';
import type { ICodeEditor, IViewZone, IViewZoneChangeAccessor } from '../../../../../editor/browser/editorBrowser.js';
import { FileChangeType, FileSystemProviderErrorCode, FileType } from '../../../../../platform/files/common/files.js';
import { INotificationService } from '../../../../../platform/notification/common/notification.js';
import { IQuickInputService } from '../../../../../platform/quickinput/common/quickInput.js';
import { WispdError } from '../../../../../platform/wisp/common/wispd.js';
import type { ContextFile } from '../../../../../platform/wisp/common/wispProtocol.js';
import { IEditorService } from '../../../../../workbench/services/editor/common/editorService.js';
import { WispContextAddFileFlow } from '../../browser/wispContextAddFile.js';
import { WispContextEditorBanner, wispContextBannerMessage, wispContextBannerTitle } from '../../browser/wispContextEditorBanner.js';
import { WispContextFileSystemProvider } from '../../browser/wispContextFileSystemProvider.js';
import { IWispContextService, WispContextService } from '../../browser/wispContextService.js';
import { contextFileDetail, projectFacts } from '../../browser/wispProjectView.js';
import type { IWispHostStatus } from '../../browser/wispHostStatus.js';
import type { IWispHostStatusService } from '../../browser/wispHostStatusService.js';
import { parseContextUri, toContextUri, validateContextFileName } from '../../common/wispContextUri.js';
import { agentsWindowServices, IAgentsWindowServices, settle } from './wispAgentsTestServices.js';
import { connected, connecting, disconnected, project, SSH_COMMAND } from './wispHostTestUtils.js';

const ONE = '0192f0c4-0000-7000-8000-000000000001';
const TWO = '0192f0c4-0000-7000-8000-000000000002';

function contextFile(path: string, options: Partial<ContextFile> = {}): ContextFile {
	return { path, size: 5, modifiedAt: '2026-09-24T12:00:00Z', lastWriter: 'editor', ...options };
}

/**
 * `assert.rejects(promise, matcher)` from upstream's own `test/unit/assert.js` treats `matcher` as
 * the failure message and never calls it, so `assert.rejects(p, (error) => error.code === X)`
 * passes no matter why `p` rejected (#106's second review, #228 for the shared fix). This awaits
 * `promise` itself and asserts on the caught error's `code`, so a wrong code actually fails.
 */
async function assertRejectsWithCode(promise: Promise<unknown>, code: string, message?: string): Promise<void> {
	try {
		await promise;
	} catch (error) {
		assert.strictEqual((error as { code?: string }).code, code, message);
		return;
	}
	assert.fail(message ? `expected ${message} to reject with ${code}` : `expected the promise to reject with ${code}`);
}

suite('wisp: shared context', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	/**
	 * Services whose wispd answers `project/list` at `seq` 9 with `ONE` and `TWO`, and `context/list`
	 * with `files`. wispd only ever serves `context/*` for a project it also lists (it checks
	 * `ensure_project_exists`), so `WispContextService`'s own watch of `IWispProjectsService.projects`
	 * needs both listed for these tests' projects to stay watched instead of pruned.
	 */
	function services(host = 'local', files: ContextFile[] = [contextFile('notes.md')]): IAgentsWindowServices & { contextService: WispContextService } {
		const context = agentsWindowServices(disposables, true, host);
		// Every answer copies its files: real IPC never hands the caller wispd's own objects, and an
		// alias here would let `upsert`'s change check see a write that already mutated its baseline.
		context.wispd.handler = async (method, params) => {
			switch (method) {
				case 'project/list':
					return { projects: [project(ONE, 'billing'), project(TWO, 'magic')], seq: 9 };
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

		test('write while the list is failed leaves the failure in place, instead of a one-file ready list', async () => {
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
			assert.strictEqual(contextService.state(ONE).get().kind, 'failed');

			await contextService.write(ONE, 'plan.md', '# Plan');
			const state = contextService.state(ONE).get();
			assert.strictEqual(state.kind, 'failed', 'the optimistic upsert did not paper over the error with a partial list');
		});

		test('write while the list is still loading does not create a one-file ready list', async () => {
			const { wispd, contextService } = services();
			let resolveList!: (result: { files: ContextFile[] }) => void;
			const listAnswer = new Promise<{ files: ContextFile[] }>(resolve => { resolveList = resolve; });
			const handler = wispd.handler!;
			wispd.handler = async (method, params) => method === 'context/list' ? listAnswer : handler(method, params);

			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'loading');

			await contextService.write(ONE, 'plan.md', '# Plan');
			assert.strictEqual(contextService.state(ONE).get().kind, 'loading', 'the optimistic upsert did not touch the loading state');

			resolveList({ files: [contextFile('notes.md')] });
			await settle();
			// The list that was already in flight, not the write, is what makes it ready.
			assert.deepStrictEqual(readyFiles(contextService, ONE).map(f => f.path), ['notes.md']);
		});

		test('a project that leaves IWispProjectsService.projects has its watch paused, and resumed if it reappears', async () => {
			const { wispd, contextService, projects } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready');
			const subscription = contextSubscription(wispd);
			assert.ok(subscription.active);

			// projects.reload() re-lists with the original handler, now answering as if ONE no longer exists.
			const originalHandler = wispd.handler!;
			let listed = [project(TWO, 'magic')];
			wispd.handler = async (method, params) => method === 'project/list' ? { projects: listed, seq: 20 } : originalHandler(method, params);
			projects.reload();
			await settle();

			assert.strictEqual(subscription.active, false, 'the watch\'s subscription ended while the project is gone');
			assert.strictEqual(contextService.state(ONE).get().kind, 'idle', 'paused, not left showing a stale ready list');

			// The Watch itself is kept, not dropped: an editor left open on one of the project's files
			// holds this same `state(ONE)` observable, and it must start reporting changes again once
			// the project comes back, without needing to be reopened (#106's second review).
			const requestsBefore = wispd.requests.filter(([method]) => method === 'context/list').length;
			listed = [project(ONE, 'billing'), project(TWO, 'magic')];
			projects.reload();
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready', 'resumed on its own once the project reappeared');
			const requestsAfter = wispd.requests.filter(([method]) => method === 'context/list').length;
			assert.strictEqual(requestsAfter, requestsBefore + 1);
		});

		test('a reconnect does not reload a paused watch for a project that no longer exists', async () => {
			const { wispd, contextService, projects } = services();
			contextService.state(ONE);
			wispd.setState(connected());
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'ready');

			// ONE leaves the list: paused, per the previous test, but its Watch stays in the map.
			const originalHandler = wispd.handler!;
			wispd.handler = async (method, params) => method === 'project/list' ? { projects: [project(TWO, 'magic')], seq: 20 } : originalHandler(method, params);
			projects.reload();
			await settle();
			assert.strictEqual(contextService.state(ONE).get().kind, 'idle');

			// A reconnect (or a host switch) used to reload every watching, idle-or-failed watch
			// unconditionally, which would ask wispd about ONE again even though its own project/list no
			// longer has it (#106's third review). It must stay idle and unasked-about.
			const listsForOne = () => wispd.requests.filter(([method, params]) => method === 'context/list' && (params as { project?: string }).project === ONE).length;
			const before = listsForOne();
			wispd.setState(disconnected('exited'));
			wispd.setState(connecting(2));
			wispd.setState(connected());
			await settle();
			assert.strictEqual(listsForOne(), before, 'ONE is not relisted: its project is gone');
			assert.strictEqual(contextService.state(ONE).get().kind, 'idle', 'still idle, not turned into a bogus ready or failed list');
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
			await assertRejectsWithCode(fsProvider.readFile(toContextUri(ONE, 'missing.md')), FileSystemProviderErrorCode.FileNotFound);
		});

		test('the project\'s folder is a directory, and writing or reading it is refused', async () => {
			const context = services();
			const fsProvider = provider(context);
			const root = URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' });
			const stat = await fsProvider.stat(root);
			assert.strictEqual(stat.type, FileType.Directory);
			await assertRejectsWithCode(fsProvider.readFile(root), FileSystemProviderErrorCode.FileIsADirectory, 'readFile');
			await assertRejectsWithCode(
				fsProvider.writeFile(root, VSBuffer.fromString('x').buffer, { create: true, overwrite: true, unlock: false, atomic: false }),
				FileSystemProviderErrorCode.FileIsADirectory, 'writeFile');
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

		/** Stubs `IQuickInputService` so the flow's one input box accepts `answer` at once. */
		function stubQuickInput(instantiationService: IAgentsWindowServices['instantiationService'], answer: string): void {
			instantiationService.stub(IQuickInputService, {
				createInputBox: () => {
					const store = new DisposableStore();
					const listeners: { accept?: () => void; hide?: () => void } = {};
					const box = {
						value: '', title: '', prompt: '', placeholder: '', ignoreFocusOut: false, validationMessage: undefined as string | undefined, severity: 0,
						onDidAccept: (fn: () => void) => { listeners.accept = fn; return store.add(toDisposable(() => { listeners.accept = undefined; })); },
						onDidChangeValue: () => store.add(toDisposable(() => { /* unused in this flow */ })),
						onDidHide: (fn: () => void) => { listeners.hide = fn; return store.add(toDisposable(() => { listeners.hide = undefined; })); },
						show: () => { box.value = answer; queueMicrotask(() => { listeners.accept?.(); listeners.hide?.(); }); },
						hide: () => listeners.hide?.(),
						dispose: () => store.dispose(),
					};
					return box;
				},
			} as unknown as IQuickInputService);
		}

		function stubOpenedEditor(instantiationService: IAgentsWindowServices['instantiationService']): URI[] {
			const opened: URI[] = [];
			instantiationService.stub(IEditorService, { openEditor: async (input: { resource: URI }) => { opened.push(input.resource); return undefined; } } as unknown as IEditorService);
			return opened;
		}

		test('validateContextFileName rejects paths, dot-files, bad extensions, empty and long names', () => {
			assert.strictEqual(validateContextFileName(''), 'Enter a file name.');
			assert.match(validateContextFileName('a/b.md') ?? '', /not a path/);
			assert.match(validateContextFileName('.hidden.md') ?? '', /dot/);
			assert.match(validateContextFileName('notes.pdf') ?? '', /\.md, \.markdown, or \.txt/);
			assert.match(validateContextFileName('notes\0.md') ?? '', /NUL/);
			assert.strictEqual(validateContextFileName('notes.md'), undefined);
			assert.strictEqual(validateContextFileName('agenda.markdown'), undefined);
			assert.strictEqual(validateContextFileName('todo.txt'), undefined);
			assert.match(validateContextFileName('a'.repeat(253) + '.md') ?? '', /at most 255 bytes/);
		});

		test('run asks for a name, writes it empty and attributed to the editor, and opens it', async () => {
			const context = services();
			context.wispd.setState(connected());
			await settle();
			const opened = stubOpenedEditor(context.instantiationService);
			stubQuickInput(context.instantiationService, 'plan.md');

			const file = await context.instantiationService.createInstance(WispContextAddFileFlow).run(ONE);
			assert.strictEqual(file?.path, 'plan.md');
			const write = context.wispd.requests.find(([method]) => method === 'context/write');
			assert.strictEqual((write?.[1] as { content: string; writer?: string }).content, '');
			assert.strictEqual((write?.[1] as { content: string; writer?: string }).writer, 'editor');
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [toContextUri(ONE, 'plan.md').toString()]);
		});

		test('typing an existing file\'s name opens it instead of overwriting it', async () => {
			const context = services(); // seeded with notes.md, per the services() default
			context.wispd.setState(connected());
			await settle();
			const opened = stubOpenedEditor(context.instantiationService);
			stubQuickInput(context.instantiationService, 'notes.md');

			const file = await context.instantiationService.createInstance(WispContextAddFileFlow).run(ONE);
			assert.strictEqual(file?.path, 'notes.md');
			assert.ok(!context.wispd.requests.some(([method]) => method === 'context/write'), 'the existing file is never overwritten');
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [toContextUri(ONE, 'notes.md').toString()]);
		});

		test('when the list is already loaded, an existing name is recognized without a context/read round trip', async () => {
			const context = services(); // seeded with notes.md, per the services() default
			context.wispd.setState(connected());
			await settle();
			context.contextService.state(ONE); // warms the watch, as the Project tab's section would have
			await settle();
			const opened = stubOpenedEditor(context.instantiationService);
			stubQuickInput(context.instantiationService, 'notes.md');
			context.wispd.requests.length = 0; // only count what the add-file flow itself sends

			const file = await context.instantiationService.createInstance(WispContextAddFileFlow).run(ONE);
			assert.strictEqual(file?.path, 'notes.md');
			assert.ok(!context.wispd.requests.some(([method]) => method === 'context/read'), 'the ready list already confirms the file exists');
			assert.ok(!context.wispd.requests.some(([method]) => method === 'context/write'));
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [toContextUri(ONE, 'notes.md').toString()]);
		});

		test('a read failure that is not contextNotFound is reported, and nothing is written', async () => {
			const context = services();
			const handler = context.wispd.handler!;
			context.wispd.handler = async (method, params) => {
				if (method === 'context/read') {
					throw new WispdError(-32603, 'the shared context folder is unavailable', undefined);
				}
				return handler(method, params);
			};
			context.wispd.setState(connected());
			await settle();
			const errors: string[] = [];
			context.instantiationService.stub(INotificationService, { error: (message: string) => errors.push(message) } as unknown as INotificationService);
			stubQuickInput(context.instantiationService, 'notes.md');

			const file = await context.instantiationService.createInstance(WispContextAddFileFlow).run(ONE);
			assert.strictEqual(file, undefined);
			assert.strictEqual(errors.length, 1);
			assert.match(errors[0], /unavailable/);
			assert.ok(!context.wispd.requests.some(([method]) => method === 'context/write'));
		});
	});

	suite('editor banner', () => {

		/** A minimal `ICodeEditor`: only the members `WispContextEditorBanner` calls. */
		function fakeEditor(initialModel: { uri: URI } | null) {
			const modelChange = new Emitter<void>();
			let model = initialModel;
			let nextId = 1;
			const zones = new Map<string, IViewZone>();
			const editor = {
				getModel: () => model,
				onDidChangeModel: modelChange.event,
				changeViewZones: (callback: (accessor: IViewZoneChangeAccessor) => void) => {
					callback({
						addZone: (zone: IViewZone) => { const id = String(nextId++); zones.set(id, zone); return id; },
						removeZone: (id: string) => { zones.delete(id); },
						layoutZone: () => { /* unused: the zone's height is fixed, never relaid out */ },
					} as IViewZoneChangeAccessor);
				},
			};
			return {
				editor: editor as unknown as ICodeEditor,
				zones,
				setModel: (next: { uri: URI } | null) => { model = next; modelChange.fire(); },
				dispose: () => modelChange.dispose(),
			};
		}

		function fakeHostStatus(host: string): IWispHostStatusService {
			return { status: observableValue<IWispHostStatus>('status', { host } as unknown as IWispHostStatus) } as unknown as IWispHostStatusService;
		}

		test('adds a zone naming the host for a wisp-context: model, and removes it for any other', () => {
			const { editor, zones, setModel, dispose } = fakeEditor({ uri: toContextUri(ONE, 'notes.md') });
			const banner = new WispContextEditorBanner(editor, fakeHostStatus('this Mac'));
			assert.strictEqual(zones.size, 1);
			assert.match([...zones.values()][0].domNode.textContent ?? '', /this Mac/);

			setModel({ uri: URI.file('/tmp/other.md') });
			assert.strictEqual(zones.size, 0, 'a plain file model gets no zone');

			setModel({ uri: toContextUri(ONE, 'notes.md') });
			assert.strictEqual(zones.size, 1, 'switching back to a wisp-context model adds one again');

			banner.dispose();
			assert.strictEqual(zones.size, 0, 'disposing the contribution removes its zone');
			dispose();
		});

		test('a plain editor model never gets a zone', () => {
			const { editor, zones, dispose } = fakeEditor({ uri: URI.file('/tmp/plain.md') });
			const banner = new WispContextEditorBanner(editor, fakeHostStatus('this Mac'));
			assert.strictEqual(zones.size, 0);
			banner.dispose();
			dispose();
		});

		test('the project\'s shared context folder itself, with no path, gets no zone', () => {
			const root = URI.from({ scheme: 'wisp-context', authority: ONE, path: '/' });
			const { editor, zones, dispose } = fakeEditor({ uri: root });
			const banner = new WispContextEditorBanner(editor, fakeHostStatus('this Mac'));
			assert.strictEqual(zones.size, 0);
			banner.dispose();
			dispose();
		});

		test('shows the short message, a fixed three-line zone, and the full sentence as a hover title', () => {
			const { editor, zones, dispose } = fakeEditor({ uri: toContextUri(ONE, 'notes.md') });
			const banner = new WispContextEditorBanner(editor, fakeHostStatus('this Mac'));
			const zone = [...zones.values()][0];

			// A fixed heightInLines, not a measured heightInPx (#106's third review): nothing about
			// this depends on the browser laying anything out first, so it can never race the moment a
			// newly added view zone is still hidden the way a dynamic, ResizeObserver-driven height did.
			assert.strictEqual(zone.heightInLines, 3);
			assert.strictEqual(zone.heightInPx, undefined);
			assert.strictEqual(zone.domNode.textContent, wispContextBannerMessage('this Mac'));
			assert.strictEqual(zone.domNode.getAttribute('title'), wispContextBannerTitle('this Mac'), 'the full sentence is reachable on hover even though the bar itself shows the shorter message');
			assert.notStrictEqual(wispContextBannerMessage('this Mac'), wispContextBannerTitle('this Mac'));

			banner.dispose();
			dispose();
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

		test('contextFileDetail reads the editor\'s own writes as "you", or just when for a file wispd never saw written', () => {
			assert.strictEqual(contextFileDetail(contextFile('notes.md', { lastWriter: 'editor', modifiedAt: new Date(Date.now() - 60_000).toISOString() })), 'you · 1 min ago');
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

		test('parseContextUri refuses a .., a subfolder, and an encoded separator: only one valid name reaches wispd', () => {
			// Constructed directly, as a stray link or another provider's URI would be, not through
			// toContextUri, which only ever builds a well-formed path.
			assert.strictEqual(parseContextUri(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/../secrets.md' })), undefined);
			assert.strictEqual(parseContextUri(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/..' })), undefined);
			assert.strictEqual(parseContextUri(URI.from({ scheme: 'wisp-context', authority: ONE, path: '/sub/notes.md' })), undefined);
			// URI.parse decodes the path, so a link spelled with %2f or %2e%2e arrives exactly like the
			// literal separator or dots above; parsing the encoded string proves that path too.
			assert.strictEqual(parseContextUri(URI.parse(`wisp-context://${ONE}/%2e%2e%2fsecrets.md`)), undefined);
			assert.strictEqual(parseContextUri(URI.parse(`wisp-context://${ONE}/sub%2fnotes.md`)), undefined);
			// A leading double slash is just an odd but harmless spelling of the same top-level name.
			assert.deepStrictEqual(parseContextUri(URI.from({ scheme: 'wisp-context', authority: ONE, path: '//notes.md' })), { project: ONE, path: 'notes.md' });
		});

		test('a bad path never reaches context/read or context/write: the provider reports FileNotFound', async () => {
			const context = services();
			const fsProvider = context.instantiationService.createInstance(WispContextFileSystemProvider);
			disposables.add(fsProvider);
			for (const bad of ['/../secrets.md', '/sub/notes.md']) {
				const uri = URI.from({ scheme: 'wisp-context', authority: ONE, path: bad });
				await assertRejectsWithCode(fsProvider.readFile(uri), FileSystemProviderErrorCode.FileNotFound, `readFile ${bad}`);
				await assertRejectsWithCode(
					fsProvider.writeFile(uri, VSBuffer.fromString('x').buffer, { create: true, overwrite: true, unlock: false, atomic: false }),
					FileSystemProviderErrorCode.FileNotFound, `writeFile ${bad}`);
			}
			assert.deepStrictEqual(context.wispd.requests, [], 'wispd never saw a request for any of them');
		});
	});
});
