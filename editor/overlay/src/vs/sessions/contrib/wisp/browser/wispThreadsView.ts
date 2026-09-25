/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispThreads.css';
import { $, addDisposableListener, append, EventType } from '../../../../base/browser/dom.js';
import { status as ariaStatus } from '../../../../base/browser/ui/aria/aria.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { MutableDisposable } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { basename } from '../../../../base/common/resources.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { IContextKeyService } from '../../../../platform/contextkey/common/contextkey.js';
import { IContextMenuService } from '../../../../platform/contextview/browser/contextView.js';
import { IHoverService } from '../../../../platform/hover/browser/hover.js';
import { IInstantiationService } from '../../../../platform/instantiation/common/instantiation.js';
import { IKeybindingService } from '../../../../platform/keybinding/common/keybinding.js';
import { IOpenerService } from '../../../../platform/opener/common/opener.js';
import { IQuickInputService } from '../../../../platform/quickinput/common/quickInput.js';
import { IThemeService } from '../../../../platform/theme/common/themeService.js';
import { IViewPaneOptions, ViewPane } from '../../../../workbench/browser/parts/views/viewPane.js';
import { IViewDescriptorService } from '../../../../workbench/common/views.js';
import { IPathService } from '../../../../workbench/services/path/common/pathService.js';
import { WISP_SHOW_HOST_MENU_COMMAND } from './wispHostMenu.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_THREADS_CONTAINER_ID = 'wisp.threads';
export const WISP_THREADS_VIEW_ID = 'wisp.threads.view';

let instanceCount = 0;

/**
 * wisp's left sidebar in the Agents window (decision record 0011, docs/design/agents-window.md in
 * wisp): actions, then Projects, Repositories, and No Repo, then a footer with the user, the host,
 * and settings. The host chip follows the wispd connection. Projects and threads come with #104,
 * so until then there are none, and New Chat is disabled.
 */
export class WispThreadsView extends ViewPane {

	private root: HTMLElement | undefined;
	private firstAction: HTMLElement | undefined;

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
		@ICommandService private readonly commandService: ICommandService,
		@IQuickInputService private readonly quickInputService: IQuickInputService,
		@IPathService private readonly pathService: IPathService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
	) {
		super(options, keybindingService, contextMenuService, configurationService, contextKeyService, viewDescriptorService, instantiationService, openerService, themeService, hoverService);
	}

	protected override renderBody(container: HTMLElement): void {
		super.renderBody(container);

		const idPrefix = `wisp-threads-${++instanceCount}`;
		const root = this.root = append(container, $('.wisp-threads'));

		const actions = append(root, $('.wisp-threads-actions', { role: 'group', 'aria-label': localize('wispThreads.actions', "Actions") }));
		const noHostReason = localize('wispThreads.newChatDisabled', "Connect to a host to start a chat.");
		this.firstAction = this.renderAction(actions, Codicon.edit, localize('wispThreads.newChat', "New Chat"), { disabledReason: noHostReason });
		this.renderAction(actions, Codicon.search, localize('wispThreads.search', "Search"), { run: () => this.search() });
		// Automations stays hidden until wisp decides on triggers (M6).
		this.renderAction(actions, Codicon.settings, localize('wispThreads.customize', "Customize"), { disabledReason: localize('wispThreads.customizeDisabled', "Host and account settings are not available yet.") });

		const lists = append(root, $('.wisp-threads-lists'));
		const projects = this.renderSection(lists, `${idPrefix}-projects`, localize('wispThreads.projects', "Projects"));
		const newProject = append(projects.header, $<HTMLButtonElement>('button.wisp-threads-icon-button', { type: 'button', 'aria-label': localize('wispThreads.newProject', "New Project") }));
		append(newProject, $(`span${ThemeIcon.asCSSSelector(Codicon.add)}`, { 'aria-hidden': 'true' }));
		this.setDisabled(newProject, localize('wispThreads.newProjectDisabled', "Connect to a host to start a project."));
		append(projects.section, $('p.wisp-threads-empty', undefined, localize('wispThreads.noProjects', "No projects yet. A project runs on a host, so it shows here once wisp connects to one.")));
		// Repositories and No Repo render only once they have threads (M3).

		this.renderFooter(root);
	}

	private renderAction(parent: HTMLElement, icon: ThemeIcon, label: string, options: { run?: () => void; disabledReason?: string }): HTMLButtonElement {
		const button = append(parent, $<HTMLButtonElement>('button.wisp-threads-action', { type: 'button' }));
		append(button, $(`span.wisp-threads-action-icon${ThemeIcon.asCSSSelector(icon)}`, { 'aria-hidden': 'true' }));
		append(button, $('span.wisp-threads-action-label', undefined, label));
		if (options.disabledReason) {
			this.setDisabled(button, options.disabledReason);
		} else if (options.run) {
			const run = options.run;
			this._register(addDisposableListener(button, EventType.CLICK, () => run()));
		}
		return button;
	}

	/**
	 * Disabled with aria-disabled rather than the disabled attribute, so the control stays in the
	 * tab order and a screen reader can read why it is disabled.
	 */
	private setDisabled(button: HTMLButtonElement, reason: string): void {
		button.classList.add('disabled');
		button.setAttribute('aria-disabled', 'true');
		button.setAttribute('aria-description', reason);
		this._register(this.hoverService.setupDelayedHover(button, { content: reason }));
	}

	private renderSection(parent: HTMLElement, id: string, title: string): { section: HTMLElement; header: HTMLElement } {
		const section = append(parent, $('section.wisp-threads-section', { 'aria-labelledby': id }));
		const header = append(section, $('.wisp-threads-section-header'));
		append(header, $('h2.wisp-threads-section-title', { id }, title));
		return { section, header };
	}

	private renderFooter(root: HTMLElement): void {
		const footer = append(root, $('.wisp-threads-footer'));

		const user = append(footer, $('.wisp-threads-user'));
		const avatar = append(user, $('span.wisp-threads-avatar', { 'aria-hidden': 'true' }));
		const name = append(user, $('span.wisp-threads-user-name'));
		this.pathService.userHome().then(home => {
			const accountName = basename(home);
			name.textContent = accountName;
			avatar.textContent = accountName.charAt(0).toUpperCase();
		});

		this.renderHostChip(footer);

		const settingsLabel = localize('wispThreads.settings', "Settings");
		const settings = append(footer, $<HTMLButtonElement>('button.wisp-threads-icon-button.wisp-threads-settings', { type: 'button', 'aria-label': settingsLabel }));
		append(settings, $(`span${ThemeIcon.asCSSSelector(Codicon.settingsGear)}`, { 'aria-hidden': 'true' }));
		this._register(this.hoverService.setupDelayedHover(settings, { content: settingsLabel }));
		this._register(addDisposableListener(settings, EventType.CLICK, () => this.commandService.executeCommand('workbench.action.openSettings')));
	}

	/**
	 * The host and its state, live from the wispd connection. The button's name carries both, so
	 * the visible parts are hidden from screen readers, and a change is announced once, politely.
	 */
	private renderHostChip(footer: HTMLElement): void {
		const chip = append(footer, $<HTMLButtonElement>('button.wisp-threads-host', { type: 'button' }));
		append(chip, $('span.wisp-threads-host-dot', { 'aria-hidden': 'true' }));
		const name = append(chip, $('span.wisp-threads-host-name', { 'aria-hidden': 'true' }));
		const state = append(chip, $('span.wisp-threads-host-state', { 'aria-hidden': 'true' }));
		const hover = this._register(new MutableDisposable());
		this._register(addDisposableListener(chip, EventType.CLICK, () => this.commandService.executeCommand(WISP_SHOW_HOST_MENU_COMMAND)));

		let announced: string | undefined;
		this._register(autorun(reader => {
			const status = this.hostStatusService.status.read(reader);
			chip.dataset.mark = status.mark;
			chip.dataset.kind = status.kind;
			chip.setAttribute('aria-label', status.ariaLabel);
			name.textContent = status.kind === 'notConnected' ? localize('wispThreads.notConnected', "Not connected") : status.host;
			state.textContent = status.showState && status.kind !== 'notConnected' ? status.state : '';
			hover.value = this.hoverService.setupDelayedHover(chip, { content: status.kind === 'error' ? `${status.ariaLabel}. ${status.heading}` : status.ariaLabel });
			// Connecting passes in a moment, so only where it lands is announced.
			if (status.kind === 'connecting') {
				return;
			}
			if (announced !== undefined && announced !== status.ariaLabel) {
				ariaStatus(status.ariaLabel);
			}
			announced = status.ariaLabel;
		}));
	}

	private async search(): Promise<void> {
		await this.quickInputService.pick([{
			label: localize('wispThreads.searchEmpty', "No projects or threads yet"),
			disabled: true,
		}], {
			placeHolder: localize('wispThreads.searchPlaceholder', "Search projects and threads"),
		});
	}

	protected override layoutBody(height: number, width: number): void {
		super.layoutBody(height, width);
		if (this.root) {
			this.root.style.height = `${height}px`;
		}
	}

	override focus(): void {
		super.focus();
		this.firstAction?.focus();
	}
}
