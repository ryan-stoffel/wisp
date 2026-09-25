/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispNoHost.css';
import { $, append } from '../../../../base/browser/dom.js';
import { Button } from '../../../../base/browser/ui/button/button.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { constObservable, IObservable } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { defaultButtonStyles } from '../../../../platform/theme/browser/defaultStyles.js';
import { AbstractCustomView } from '../../../services/customView/browser/customView.js';

export const WISP_NO_HOST_VIEW_ID = 'wisp.agents.noHost';

let instanceCount = 0;

/**
 * Covers the session surface while no host is connected (docs/design/agents-window.md in wisp,
 * M0). The composer is read-only and aria-disabled, so it stays in the tab order and is described
 * by the reason.
 */
export class WispNoHostView extends AbstractCustomView {

	// The body has its own heading, so the host's header is hidden (media/wispNoHost.css). An empty
	// title also leaves the host's region unnamed, so only this view's region is a landmark.
	readonly title: IObservable<string> = constObservable('');
	override readonly maxWidth = 600;

	private root: HTMLElement | undefined;
	private input: HTMLTextAreaElement | undefined;

	render(container: HTMLElement): void {
		const idPrefix = `wisp-no-host-${++instanceCount}`;
		const heading = localize('wispNoHost.heading', "No host connected");
		const root = this.root = append(container, $('.wisp-agents-no-host', { role: 'region', 'aria-label': heading }));

		const message = append(root, $('.wisp-no-host-message'));
		append(message, $('h2.wisp-no-host-heading', { id: `${idPrefix}-heading` }, heading));
		append(message, $('p.wisp-no-host-body', { id: `${idPrefix}-body` }, localize('wispNoHost.body', "The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.")));

		const composer = append(root, $('.wisp-no-host-composer'));
		const attach = append(composer, $('span.wisp-no-host-attach', { 'aria-hidden': 'true' }));
		append(attach, $(`span${ThemeIcon.asCSSSelector(Codicon.add)}`));
		this.input = append(composer, $<HTMLTextAreaElement>('textarea.wisp-no-host-input', {
			rows: '1',
			readonly: '',
			placeholder: localize('wispNoHost.placeholder', "Not connected to a host"),
			'aria-label': localize('wispNoHost.inputLabel', "Message the coordinator"),
			'aria-disabled': 'true',
			'aria-describedby': `${idPrefix}-heading ${idPrefix}-body`,
		}));
		const sendLabel = localize('wispNoHost.send', "Send");
		const send = this._register(new Button(composer, { ...defaultButtonStyles, title: sendLabel, ariaLabel: sendLabel, disabled: true }));
		send.element.classList.add('wisp-no-host-send');
		send.icon = Codicon.arrowUp;

		// The branch and host of the thread, as the composer footer shows them once a host connects.
		// The heading already says there is no host, so screen readers skip it.
		const footer = append(root, $('.wisp-no-host-composer-footer', { 'aria-hidden': 'true' }));
		const branch = append(footer, $('span.wisp-no-host-branch'));
		append(branch, $(`span${ThemeIcon.asCSSSelector(Codicon.gitBranch)}`));
		append(branch, $('span', undefined, '-'));
		const host = append(footer, $('span.wisp-no-host-host'));
		append(host, $(`span${ThemeIcon.asCSSSelector(Codicon.server)}`));
		append(host, $('span', undefined, localize('wispNoHost.notConnected', "Not connected")));
	}

	layout(_width: number, height: number): void {
		if (this.root) {
			this.root.style.minHeight = `${height}px`;
		}
	}

	override focus(): void {
		this.input?.focus();
	}
}
