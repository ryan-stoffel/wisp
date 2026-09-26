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
import { toContextUri, validateContextFileName } from '../common/wispContextUri.js';
import { IWispContextService } from './wispContextService.js';

/**
 * Adds a shared context file (docs/design/agents-window.md, note 10; decision record 0005): asks
 * for its name, and either opens it if it already exists, or creates it empty with `context/write`
 * and opens it. A name the watched `ready` list already has is known to exist without a round trip
 * to wispd for its content; otherwise (the list isn't `ready` yet, or doesn't list the name) this
 * confirms with `context/read`, since the list can be stale by the width of one write from another
 * agent. That leaves a small window, between a `contextNotFound` answer and the write below, where
 * such a write could still be overwritten; wispd has no create-only mode for `context/write` to
 * close it.
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
		let existing = this.knownExisting(project, name);
		if (!existing) {
			try {
				existing = (await this.contextService.read(project, name)).file;
			} catch (error) {
				if (!(error instanceof WispdError && error.kind === 'contextNotFound')) {
					this.notifyFailure(name, error);
					return undefined;
				}
			}
		}
		if (existing) {
			await this.editorService.openEditor({ resource: toContextUri(project, name) });
			return existing;
		}
		let file: ContextFile;
		try {
			file = await this.contextService.write(project, name, '');
		} catch (error) {
			this.notifyFailure(name, error);
			return undefined;
		}
		await this.editorService.openEditor({ resource: toContextUri(project, name) });
		return file;
	}

	/** A name the watched `ready` list already has, without a `context/read` round trip. */
	private knownExisting(project: ProjectId, name: string): ContextFile | undefined {
		const state = this.contextService.state(project).get();
		return state.kind === 'ready' ? state.files.find(file => file.path === name) : undefined;
	}

	private notifyFailure(name: string, error: unknown): void {
		const message = error instanceof WispdError || error instanceof WispdUnavailableError
			? error.message
			: error instanceof Error ? error.message : String(error);
		this.notificationService.error(localize('wispContext.addFailed', "Couldn't add {0}: {1}", name, message));
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
