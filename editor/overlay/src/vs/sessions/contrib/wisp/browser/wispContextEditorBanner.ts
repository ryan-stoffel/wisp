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
	private resizeObserver: ResizeObserver | undefined;
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
		this.zoneStore.add(autorun(reader => {
			const status = this.hostStatusService.status.read(reader);
			this.renderZone(status.host);
		}));
	}

	private renderZone(host: string): void {
		this.removeZone();
		const message = wispContextBannerMessage(host);
		// A dedicated inner element to measure, kept apart from the domNode the zone infrastructure
		// itself owns and positions: upstream's view zones add the zone hidden and only show it on the
		// next render (editor/vscode's viewZones.ts), so the zone's own domNode measures 0px at the
		// moment addZone returns, and the editor then keeps whatever heightInPx was passed as the
		// guess. Measuring here is deferred to a ResizeObserver on the inner element instead, which
		// fires once for that hidden-to-shown transition (0 to its real wrapped height) and again on
		// every width change afterwards (the pane resizing, wrapping the sentence differently),
		// covering both "re-measure once it's shown" and "re-measure on width changes" the same way
		// (#106's third review). Until the first callback lands, the outer domNode clips to whatever
		// height is current, so a still-wrong guess hides extra lines instead of drawing them over the
		// file's own content below.
		const inner = $('.wisp-context-banner-text', undefined, message);
		const domNode = $('.wisp-context-banner', { role: 'note', title: message }, inner);
		const zone: IViewZone = { afterLineNumber: 0, heightInPx: 28, domNode };
		this.zone = zone;
		this.editor.changeViewZones(accessor => {
			this.zoneId = accessor.addZone(zone);
		});
		// removeZone() above already disconnected and cleared any previous observer.
		this.resizeObserver = new ResizeObserver(() => this.resize(inner));
		this.resizeObserver.observe(inner);
	}

	/** Relays out the zone to the inner element's current (natural, wrapped) height, if it changed. */
	private resize(inner: HTMLElement): void {
		if (this.zoneId === undefined || !this.zone) {
			return;
		}
		const height = inner.offsetHeight;
		if (!height || height === this.zone.heightInPx) {
			return;
		}
		this.zone.heightInPx = height;
		const id = this.zoneId;
		this.editor.changeViewZones(accessor => accessor.layoutZone(id));
	}

	private removeZone(): void {
		this.resizeObserver?.disconnect();
		this.resizeObserver = undefined;
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
