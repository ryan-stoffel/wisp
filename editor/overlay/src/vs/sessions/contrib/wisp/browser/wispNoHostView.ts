/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispNoHost.css';
import { $, append, clearNode } from '../../../../base/browser/dom.js';
import { Button } from '../../../../base/browser/ui/button/button.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { autorun, constObservable, IObservable } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { defaultButtonStyles } from '../../../../platform/theme/browser/defaultStyles.js';
import { hostLabel } from '../../../../platform/wisp/common/wispdConfiguration.js';
import { AbstractCustomView } from '../../../services/customView/browser/customView.js';
import { WISP_RETRY_COMMAND, WISP_SHOW_LOG_COMMAND, WISP_SWITCH_HOST_COMMAND } from './wispHostMenu.js';
import { IWispHostStatus, WispHostAction } from './wispHostStatus.js';
import { IWispHostStatusService, LOCAL_HOST_LABEL } from './wispHostStatusService.js';

export const WISP_NO_HOST_VIEW_ID = 'wisp.agents.noHost';

const WISP_SETTINGS_QUERY = '@id:wisp.*';

let instanceCount = 0;

/**
 * Covers the session surface while no host is connected (docs/design/agents-window.md in wisp):
 * why, in the spec's words for the connection state, what to do about it, and a composer that is
 * read-only and aria-disabled, so it stays in the tab order and is described by the reason.
 */
export class WispNoHostView extends AbstractCustomView {

	// The body has its own heading, so the host's header is hidden (media/wispNoHost.css). An empty
	// title also leaves the host's region unnamed, so only this view's region is a landmark.
	readonly title: IObservable<string> = constObservable('');
	override readonly maxWidth = 600;

	private root: HTMLElement | undefined;
	private input: HTMLTextAreaElement | undefined;
	private renderedActions: string | undefined;
	private readonly actionButtons = new Map<WispHostAction, Button>();

	constructor(
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@ICommandService private readonly commandService: ICommandService,
	) {
		super();
	}

	render(container: HTMLElement): void {
		const idPrefix = `wisp-no-host-${++instanceCount}`;
		const root = this.root = append(container, $('.wisp-agents-no-host', { role: 'region' }));

		const message = append(root, $('.wisp-no-host-message'));
		const heading = append(message, $('h2.wisp-no-host-heading', { id: `${idPrefix}-heading` }));
		const body = append(message, $('p.wisp-no-host-body', { id: `${idPrefix}-body` }));
		const actions = append(message, $('.wisp-no-host-actions'));
		const actionStore = this._register(new DisposableStore());

		const composer = append(root, $('.wisp-no-host-composer'));
		const attach = append(composer, $('span.wisp-no-host-attach', { 'aria-hidden': 'true' }));
		append(attach, $(`span${ThemeIcon.asCSSSelector(Codicon.add)}`));
		const input = this.input = append(composer, $<HTMLTextAreaElement>('textarea.wisp-no-host-input', {
			rows: '1',
			readonly: '',
			'aria-disabled': 'true',
			'aria-describedby': `${idPrefix}-heading ${idPrefix}-body`,
		}));
		const sendLabel = localize('wispNoHost.send', "Send");
		const send = this._register(new Button(composer, { ...defaultButtonStyles, title: sendLabel, ariaLabel: sendLabel, disabled: true }));
		send.element.classList.add('wisp-no-host-send');
		send.icon = Codicon.arrowUp;

		// The thread's branch and host, as the composer footer shows them. There is no thread yet, so
		// the branch is a dash and the host is the configured one. The composer's label names the host
		// for screen readers, so the footer is hidden from them.
		const footer = append(root, $('.wisp-no-host-composer-footer', { 'aria-hidden': 'true' }));
		const branch = append(footer, $('span.wisp-no-host-branch'));
		append(branch, $(`span${ThemeIcon.asCSSSelector(Codicon.gitBranch)}`));
		append(branch, $('span', undefined, '-'));
		const hostItem = append(footer, $('span.wisp-no-host-host'));
		append(hostItem, $(`span${ThemeIcon.asCSSSelector(Codicon.server)}`));
		const hostName = append(hostItem, $('span.wisp-no-host-host-name'));

		this._register(autorun(reader => {
			const status = this.hostStatusService.status.read(reader);
			const host = hostLabel(this.hostStatusService.configuredHost.read(reader), LOCAL_HOST_LABEL);

			root.dataset.kind = status.kind;
			root.setAttribute('aria-label', status.heading);
			heading.textContent = status.heading;
			body.textContent = status.body;
			input.placeholder = status.placeholder;
			input.setAttribute('aria-label', localize('wispNoHost.inputLabel', "Message the coordinator on {0}", host));
			hostName.textContent = host;
			this.renderActions(actions, actionStore, status);
		}));
	}

	/**
	 * Rebuilds the buttons only when the set of actions changes, so a focused button keeps focus
	 * while its label follows the state (Retry now, then Retrying... while the client reconnects).
	 */
	private renderActions(container: HTMLElement, store: DisposableStore, status: IWispHostStatus): void {
		const key = status.actions.join(',');
		if (key !== this.renderedActions) {
			this.renderedActions = key;
			store.clear();
			clearNode(container);
			this.actionButtons.clear();
			status.actions.forEach((action, index) => {
				const button = store.add(new Button(container, { ...defaultButtonStyles, secondary: index > 0 }));
				button.element.classList.add('wisp-no-host-action');
				button.element.dataset.action = action;
				store.add(button.onDidClick(() => this.run(action)));
				this.actionButtons.set(action, button);
			});
		}
		container.hidden = status.actions.length === 0;
		for (const [action, button] of this.actionButtons) {
			button.label = actionLabel(action, status.retrying);
		}
	}

	private run(action: WispHostAction): void {
		switch (action) {
			case 'retry': this.commandService.executeCommand(WISP_RETRY_COMMAND); break;
			case 'switchHost': this.commandService.executeCommand(WISP_SWITCH_HOST_COMMAND); break;
			case 'openSettings': this.commandService.executeCommand('workbench.action.openSettings', WISP_SETTINGS_QUERY); break;
			case 'showLog': this.commandService.executeCommand(WISP_SHOW_LOG_COMMAND); break;
		}
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

function actionLabel(action: WispHostAction, retrying: boolean): string {
	switch (action) {
		case 'retry': return retrying ? localize('wispNoHost.retrying', "Retrying...") : localize('wispNoHost.retry', "Retry now");
		case 'switchHost': return localize('wispNoHost.switchHost', "Switch host...");
		case 'openSettings': return localize('wispNoHost.openSettings', "Open Settings");
		case 'showLog': return localize('wispNoHost.showLog', "Show log");
	}
}
