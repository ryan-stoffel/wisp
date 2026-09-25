/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispProject.css';
import { $, append } from '../../../../base/browser/dom.js';
import { BaseActionViewItem } from '../../../../base/browser/ui/actionbar/actionViewItems.js';
import { IAction } from '../../../../base/common/actions.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { Disposable, MutableDisposable } from '../../../../base/common/lifecycle.js';
import { autorun, observableValue } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { URI } from '../../../../base/common/uri.js';
import { localize, localize2 } from '../../../../nls.js';
import { IActionViewItemService } from '../../../../platform/actions/browser/actionViewItemService.js';
import { Action2, MenuId, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { IInstantiationService, ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { isLocalHost } from '../../../../platform/wisp/common/wispdConfiguration.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import type { IChatExecuteActionContext } from '../../../../workbench/contrib/chat/browser/actions/chatExecuteActions.js';
import { ChatContextKeys } from '../../../../workbench/contrib/chat/common/actions/chatContextKeys.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { projectIdOf, WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';
import { WISP_SHOW_HOST_MENU_COMMAND } from './wispHostMenu.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_COMPOSER_BRANCH_ACTION = 'wisp.composer.branch';
export const WISP_COMPOSER_HOST_ACTION = 'wisp.composer.host';

const inProject = ChatContextKeys.chatSessionType.isEqualTo(WISP_PROJECT_SESSION_TYPE);

// The composer footer of a project's thread (docs/design/agents-window.md, note 9): the branch its
// repository has checked out, then the host it runs on, which opens the host menu.

registerAction2(class ComposerBranchAction extends Action2 {
	constructor() {
		super({
			id: WISP_COMPOSER_BRANCH_ACTION,
			title: localize2('wispComposer.branch', "Branch"),
			icon: Codicon.gitBranch,
			menu: { id: MenuId.ChatInputSecondary, group: 'navigation', order: 1000, when: inProject },
		});
	}

	run(): void { }
});

registerAction2(class ComposerHostAction extends Action2 {
	constructor() {
		super({
			id: WISP_COMPOSER_HOST_ACTION,
			title: localize2('wispComposer.host', "Host"),
			icon: Codicon.server,
			menu: { id: MenuId.ChatInputSecondary, group: 'navigation', order: 1001, when: inProject },
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		await accessor.get(ICommandService).executeCommand(WISP_SHOW_HOST_MENU_COMMAND);
	}
});

type FooterKind = 'branch' | 'host';

/**
 * One footer item: an icon and a short text. The branch is information only, so it is not in the
 * tab order; the host is a button, like the sidebar's host chip.
 */
export class WispComposerFooterItem extends BaseActionViewItem {

	private readonly sessionResource = observableValue<URI | undefined>(this, undefined);
	private readonly widgetListener = this._register(new MutableDisposable());

	constructor(
		action: IAction,
		private readonly kind: FooterKind,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
	) {
		super(undefined, action);
	}

	override setActionContext(context: unknown): void {
		super.setActionContext(context);
		const widget = (context as IChatExecuteActionContext | undefined)?.widget;
		this.sessionResource.set(widget?.viewModel?.sessionResource, undefined);
		this.widgetListener.value = widget?.onDidChangeViewModel(() => this.sessionResource.set(widget.viewModel?.sessionResource, undefined));
	}

	override render(container: HTMLElement): void {
		super.render(container);
		container.classList.add('wisp-composer-footer-item', `wisp-composer-footer-${this.kind}`);
		const icon = append(container, $('span.wisp-composer-footer-icon', { 'aria-hidden': 'true' }));
		const text = append(container, $('span.wisp-composer-footer-text', { 'aria-hidden': 'true' }));
		if (this.kind === 'host') {
			container.setAttribute('role', 'button');
			container.tabIndex = 0;
		} else {
			container.tabIndex = -1;
		}

		this._register(autorun(reader => {
			const id = this.sessionResource.read(reader) && projectIdOf(this.sessionResource.read(reader)!);
			const project = id ? this.projectsService.projects.read(reader).find(candidate => candidate.id === id) : undefined;
			const status = this.hostStatusService.status.read(reader);
			const local = isLocalHost(this.hostStatusService.configuredHost.read(reader));
			let iconId: ThemeIcon;
			let label: string;
			let ariaLabel: string;
			if (this.kind === 'branch') {
				iconId = Codicon.gitBranch;
				label = project?.branch ?? localize('wispComposer.noBranch', "no branch");
				ariaLabel = project?.branch
					? localize('wispComposer.branchAria', "Branch: {0}", project.branch)
					: localize('wispComposer.noBranchAria', "Branch: unknown");
			} else {
				iconId = local ? Codicon.deviceDesktop : Codicon.server;
				label = status.host;
				ariaLabel = status.ariaLabel;
			}
			icon.className = `wisp-composer-footer-icon ${ThemeIcon.asClassName(iconId)}`;
			text.textContent = label;
			container.setAttribute('aria-label', ariaLabel);
			container.title = ariaLabel;
			container.hidden = this.kind === 'branch' && !project;
		}));
	}

	override focus(): void {
		if (this.kind === 'host') {
			this.element?.focus();
		}
	}

	override isFocused(): boolean {
		return this.kind === 'host' && !!this.element && this.element.ownerDocument.activeElement === this.element;
	}
}

export class WispComposerFooterContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispComposerFooter';

	constructor(
		@IActionViewItemService actionViewItemService: IActionViewItemService,
		@IInstantiationService instantiationService: IInstantiationService,
	) {
		super();
		for (const [id, kind] of [[WISP_COMPOSER_BRANCH_ACTION, 'branch'], [WISP_COMPOSER_HOST_ACTION, 'host']] as const) {
			this._register(actionViewItemService.register(MenuId.ChatInputSecondary, id, action => instantiationService.createInstance(WispComposerFooterItem, action, kind)));
		}
	}
}

registerWorkbenchContribution2(WispComposerFooterContribution.ID, WispComposerFooterContribution, WorkbenchPhase.BlockRestore);
