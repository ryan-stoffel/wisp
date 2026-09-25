/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/coordinatorChat.css';
import { $, append } from '../../../../base/browser/dom.js';
import { Button } from '../../../../base/browser/ui/button/button.js';
import { localize } from '../../../../nls.js';
import { IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { IContextKeyService } from '../../../../platform/contextkey/common/contextkey.js';
import { IContextMenuService } from '../../../../platform/contextview/browser/contextView.js';
import { IHoverService } from '../../../../platform/hover/browser/hover.js';
import { IInstantiationService } from '../../../../platform/instantiation/common/instantiation.js';
import { IKeybindingService } from '../../../../platform/keybinding/common/keybinding.js';
import { IOpenerService } from '../../../../platform/opener/common/opener.js';
import { defaultButtonStyles } from '../../../../platform/theme/browser/defaultStyles.js';
import { IThemeService } from '../../../../platform/theme/common/themeService.js';
import { IViewPaneOptions, ViewPane } from '../../../browser/parts/views/viewPane.js';
import { IViewDescriptorService } from '../../../common/views.js';

export const COORDINATOR_VIEW_CONTAINER_ID = 'wisp.coordinator';
export const COORDINATOR_CHAT_VIEW_ID = 'wisp.coordinatorChat';

let instanceCount = 0;

/**
 * The coordinator chat. Until the editor connects to wispd (#63), it shows the empty state for
 * no host, with the composer read-only (docs/design/coordinator-chat.md in wisp).
 */
export class CoordinatorChatViewPane extends ViewPane {

	private root: HTMLElement | undefined;
	private composerInput: HTMLTextAreaElement | undefined;

	constructor(
		options: IViewPaneOptions,
		@IKeybindingService keybindingService: IKeybindingService,
		@IContextMenuService contextMenuService: IContextMenuService,
		@IConfigurationService configurationService: IConfigurationService,
		@IContextKeyService contextKeyService: IContextKeyService,
		@IViewDescriptorService viewDescriptorService: IViewDescriptorService,
		@IInstantiationService instantiationService: IInstantiationService,
		@IOpenerService openerService: IOpenerService,
		@IThemeService themeService: IThemeService,
		@IHoverService hoverService: IHoverService,
	) {
		super(options, keybindingService, contextMenuService, configurationService, contextKeyService, viewDescriptorService, instantiationService, openerService, themeService, hoverService);
	}

	protected override renderBody(container: HTMLElement): void {
		super.renderBody(container);

		const idPrefix = `wisp-coordinator-chat-${++instanceCount}`;
		const root = this.root = append(container, $('.wisp-coordinator-chat', {
			role: 'region',
			'aria-label': localize('coordinatorChat', "Coordinator chat"),
		}));

		const context = append(root, $('.wisp-chat-context', {
			role: 'group',
			'aria-label': localize('projectAndHost', "Project and host"),
		}));
		append(context, $('span.wisp-chat-project', undefined, localize('noProject', "No project")));
		const host = append(context, $('span.wisp-chat-host'));
		append(host, $('span.wisp-chat-dot', { 'aria-hidden': 'true' }));
		append(host, $('span.wisp-chat-host-label', undefined, localize('notConnected', "Not connected")));

		const empty = append(root, $('.wisp-chat-empty'));
		append(empty, $('h3', { id: `${idPrefix}-heading` }, localize('noHostHeading', "No host connected")));
		append(empty, $('p', { id: `${idPrefix}-body` }, localize('noHostBody', "The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.")));

		const composer = append(root, $('.wisp-chat-composer'));
		this.composerInput = append(composer, $<HTMLTextAreaElement>('textarea.wisp-chat-input', {
			rows: '2',
			readonly: '',
			placeholder: localize('notConnectedToHost', "Not connected to a host"),
			'aria-label': localize('messageCoordinator', "Message the coordinator"),
			'aria-disabled': 'true',
			'aria-describedby': `${idPrefix}-heading ${idPrefix}-body`,
		}));

		const footer = append(composer, $('.wisp-chat-composer-footer'));
		append(footer, $('.wisp-chat-account'));
		const send = this._register(new Button(footer, { ...defaultButtonStyles, disabled: true }));
		send.element.classList.add('wisp-chat-send');
		send.label = localize('send', "Send");
	}

	protected override layoutBody(height: number, width: number): void {
		super.layoutBody(height, width);
		if (this.root) {
			this.root.style.height = `${height}px`;
		}
	}

	override focus(): void {
		super.focus();
		this.composerInput?.focus();
	}
}
