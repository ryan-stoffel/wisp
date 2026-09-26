/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispContext.css';
import { $ } from '../../../../base/browser/dom.js';
import { Disposable, DisposableStore } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { localize } from '../../../../nls.js';
import { ICodeEditor, IViewZone } from '../../../../editor/browser/editorBrowser.js';
import { EditorContributionInstantiation, registerEditorContribution } from '../../../../editor/browser/editorExtensions.js';
import { IEditorContribution } from '../../../../editor/common/editorCommon.js';
import { parseContextUri } from '../common/wispContextUri.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_CONTEXT_EDITOR_BANNER_ID = 'wisp.contrib.contextEditorBanner';

/** The bar's text (docs/design/agents-window.md, note 10; issue #106's acceptance criteria). */
export function wispContextBannerMessage(host: string): string {
	return localize('wispContext.banner', "This file lives on {0}, outside the repo. Every agent in the project reads it.", host);
}

/**
 * A view zone above a shared context file's content, naming the host it lives on (note 10). Only
 * `wisp-context:` resources get one; every other file is unaffected.
 */
export class WispContextEditorBanner extends Disposable implements IEditorContribution {

	static readonly ID = WISP_CONTEXT_EDITOR_BANNER_ID;

	private zoneId: string | undefined;
	private zone: IViewZone | undefined;
	private readonly zoneStore = this._register(new DisposableStore());

	constructor(
		private readonly editor: ICodeEditor,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
	) {
		super();
		this._register(editor.onDidChangeModel(() => this.update()));
		this.update();
	}

	private update(): void {
		this.zoneStore.clear();
		this.removeZone();
		const model = this.editor.getModel();
		const parsed = model ? parseContextUri(model.uri) : undefined;
		if (!parsed?.path) {
			return;
		}
		// The detail pane is often narrower than the sentence needs (round two of #106's review), so
		// the zone's height follows the domNode's own wrapped height, not a fixed guess, and is kept
		// in sync as the editor's width changes (the pane resizing, or the window itself).
		this.zoneStore.add(this.editor.onDidLayoutChange(() => this.resize()));
		this.zoneStore.add(autorun(reader => {
			const status = this.hostStatusService.status.read(reader);
			this.renderZone(status.host);
		}));
	}

	private renderZone(host: string): void {
		this.removeZone();
		const message = wispContextBannerMessage(host);
		const domNode = $('.wisp-context-banner', { role: 'note', title: message }, message);
		const zone: IViewZone = { afterLineNumber: 0, heightInPx: 28, domNode };
		this.zone = zone;
		this.editor.changeViewZones(accessor => {
			this.zoneId = accessor.addZone(zone);
		});
		this.resize();
	}

	/** Measures the domNode's own (wrapped) height and relays out the zone if it changed. */
	private resize(): void {
		if (this.zoneId === undefined || !this.zone) {
			return;
		}
		const height = this.zone.domNode.offsetHeight;
		if (!height || height === this.zone.heightInPx) {
			return;
		}
		this.zone.heightInPx = height;
		const id = this.zoneId;
		this.editor.changeViewZones(accessor => accessor.layoutZone(id));
	}

	private removeZone(): void {
		if (this.zoneId === undefined) {
			return;
		}
		const id = this.zoneId;
		this.zoneId = undefined;
		this.zone = undefined;
		this.editor.changeViewZones(accessor => accessor.removeZone(id));
	}

	override dispose(): void {
		this.removeZone();
		super.dispose();
	}
}

registerEditorContribution(WispContextEditorBanner.ID, WispContextEditorBanner, EditorContributionInstantiation.AfterFirstRender);
