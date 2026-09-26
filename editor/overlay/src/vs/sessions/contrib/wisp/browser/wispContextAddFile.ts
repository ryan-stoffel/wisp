/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { DisposableStore } from '../../../../base/common/lifecycle.js';
import Severity from '../../../../base/common/severity.js';
import { localize } from '../../../../nls.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService } from '../../../../platform/quickinput/common/quickInput.js';
import { WispdError, WispdUnavailableError } from '../../../../platform/wisp/common/wispd.js';
import type { ContextFile, ProjectId } from '../../../../platform/wisp/common/wispProtocol.js';
import { IEditorService } from '../../../../workbench/services/editor/common/editorService.js';
import { toContextUri } from '../common/wispContextUri.js';
import { IWispContextService } from './wispContextService.js';

/** wispd's limit on a shared context file's name (0005, #155). */
const MAX_NAME_BYTES = 255;
const EXTENSIONS = ['.md', '.markdown', '.txt'];
const encoder = new TextEncoder();

/**
 * wispd only ever holds Markdown or plain text in a project's shared context: one file name, no
 * path, no leading dot, and one of the extensions it accepts.
 */
export function validateContextFileName(name: string): string | undefined {
	if (name.length === 0) {
		return localize('wispContext.nameEmpty', "Enter a file name.");
	}
	if (name.includes('/') || name.includes('\\')) {
		return localize('wispContext.nameSlash', "Enter a file name, not a path.");
	}
	if (name.startsWith('.')) {
		return localize('wispContext.nameDot', "Enter a name that doesn't start with a dot.");
	}
	if (!EXTENSIONS.some(extension => name.endsWith(extension))) {
		return localize('wispContext.nameExtension', "Enter a name ending in .md, .markdown, or .txt.");
	}
	if (encoder.encode(name).length > MAX_NAME_BYTES) {
		return localize('wispContext.nameLength', "Enter a name of at most {0} bytes.", MAX_NAME_BYTES);
	}
	return undefined;
}

/**
 * Adds a shared context file (docs/design/agents-window.md, note 10; decision record 0005): asks
 * for its name, creates it empty with `context/write`, and opens it in the editor.
 */
export class WispContextAddFileFlow {

	constructor(
		@IQuickInputService private readonly quickInputService: IQuickInputService,
		@IWispContextService private readonly contextService: IWispContextService,
		@IEditorService private readonly editorService: IEditorService,
		@INotificationService private readonly notificationService: INotificationService,
	) { }

	async run(project: ProjectId): Promise<ContextFile | undefined> {
		const name = await this.ask();
		if (name === undefined) {
			return undefined;
		}
		let file: ContextFile;
		try {
			file = await this.contextService.write(project, name, '');
		} catch (error) {
			const message = error instanceof WispdError || error instanceof WispdUnavailableError
				? error.message
				: error instanceof Error ? error.message : String(error);
			this.notificationService.error(localize('wispContext.addFailed', "Couldn't add {0}: {1}", name, message));
			return undefined;
		}
		await this.editorService.openEditor({ resource: toContextUri(project, name) });
		return file;
	}

	private ask(): Promise<string | undefined> {
		const store = new DisposableStore();
		const box = store.add(this.quickInputService.createInputBox());
		box.title = localize('wispContext.addTitle', "Add Shared Context File");
		box.prompt = localize('wispContext.addPrompt', "A file name every agent in the project can read, such as notes.md");
		box.placeholder = 'notes.md';
		box.ignoreFocusOut = true;
		const setProblem = (message: string | undefined) => {
			box.validationMessage = message;
			box.severity = message ? Severity.Error : Severity.Ignore;
		};
		return new Promise<string | undefined>(resolve => {
			let accepted: string | undefined;
			store.add(box.onDidChangeValue(() => setProblem(undefined)));
			store.add(box.onDidAccept(() => {
				const text = box.value.trim();
				const problem = validateContextFileName(text);
				if (problem) {
					setProblem(problem);
					return;
				}
				accepted = text;
				box.hide();
			}));
			store.add(box.onDidHide(() => {
				store.dispose();
				resolve(accepted);
			}));
			box.show();
		});
	}
}
