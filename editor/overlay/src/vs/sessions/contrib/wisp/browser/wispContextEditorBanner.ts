/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispContext.css';
import { $ } from '../../../../base/browser/dom.js';
import { Disposable, DisposableStore } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { localize } from '../../../../nls.js';
import { ICodeEditor } from '../../../../editor/browser/editorBrowser.js';
import { EditorContributionInstantiation, registerEditorContribution } from '../../../../editor/browser/editorExtensions.js';
import { IEditorContribution } from '../../../../editor/common/editorCommon.js';
import { parseContextUri } from '../common/wispContextUri.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_CONTEXT_EDITOR_BANNER_ID = 'wisp.contrib.contextEditorBanner';

/**
 * How many lines the bar reserves, fixed rather than measured (#106's third review). A dynamic,
 * ResizeObserver-driven height raced upstream's own hidden-then-shown cycle for a newly added view
 * zone: the browser could still paint the zone at its initial guess before the observer's
 * correction landed, so the packaged app's screenshot caught the file's own first line drawn over
 * the bar's text, even though the same logic passed in a unit test that fakes the observer's
 * timing. A fixed line count sidesteps that race entirely: nothing about it depends on the browser
 * laying anything out first.
 */
const BANNER_LINES = 2;

/**
 * The bar's text (docs/design/agents-window.md, note 10), short enough to fit {@link BANNER_LINES}
 * lines at the detail pane's minimum width. The full sentence is still reachable as the zone's
 * hover title ({@link wispContextBannerTitle}).
 */
export function wispContextBannerMessage(host: string): string {
	return localize('wispContext.banner', "Lives on {0}, outside the repo. Every agent in this project reads it.", host);
}

/** The full sentence (issue #106's acceptance criteria), as the zone's hover title. */
export function wispContextBannerTitle(host: string): string {
	return localize('wispContext.bannerTitle', "This file lives on {0}, outside the repo. Every agent in the project reads it.", host);
}

/**
 * A view zone above a shared context file's content, naming the host it lives on (note 10). Only
 * `wisp-context:` resources get one; every other file is unaffected.
 */
export class WispContextEditorBanner extends Disposable implements IEditorContribution {

	static readonly ID = WISP_CONTEXT_EDITOR_BANNER_ID;

	private zoneId: string | undefined;
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
		const domNode = $('.wisp-context-banner', { role: 'note', title: wispContextBannerTitle(host) }, wispContextBannerMessage(host));
		this.editor.changeViewZones(accessor => {
			this.zoneId = accessor.addZone({ afterLineNumber: 0, heightInLines: BANNER_LINES, domNode });
		});
	}

	private removeZone(): void {
		if (this.zoneId === undefined) {
			return;
		}
		const id = this.zoneId;
		this.zoneId = undefined;
		this.editor.changeViewZones(accessor => accessor.removeZone(id));
	}

	override dispose(): void {
		this.removeZone();
		super.dispose();
	}
}

registerEditorContribution(WispContextEditorBanner.ID, WispContextEditorBanner, EditorContributionInstantiation.AfterFirstRender);
