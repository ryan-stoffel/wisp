/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispProject.css';
import { $, append } from '../../../../base/browser/dom.js';
import { BaseActionViewItem } from '../../../../base/browser/ui/actionbar/actionViewItems.js';
import { IAction } from '../../../../base/common/actions.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { Disposable } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, MenuId, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { IInstantiationService, ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService, IQuickPickItem, IQuickPickSeparator } from '../../../../platform/quickinput/common/quickInput.js';
import { IActionViewItemService } from '../../../../platform/actions/browser/actionViewItemService.js';
import { WispdError } from '../../../../platform/wisp/common/wispd.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { ChatContextKeys } from '../../../../workbench/contrib/chat/common/actions/chatContextKeys.js';
import { WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';
import { cliAccountLabel, IWispAccountsService, keyAccountLabel, resolveAccountLabel, WispAccountChoice } from './wispAccounts.js';
import { WISP_SHOW_ACCOUNTS_COMMAND } from './wispAccountsEditor.js';

export const WISP_COMPOSER_ACCOUNT_ACTION = 'wisp.composer.account';
export const WISP_PICK_ACCOUNT_COMMAND = 'wisp.accounts.pick';

const category = localize2('wisp', "Wisp");
const inProject = ChatContextKeys.chatSessionType.isEqualTo(WISP_PROJECT_SESSION_TYPE);

// The composer's account picker (docs/design/agents-window.md: "Claude Code: Max" in the
// mockups), in the same input toolbar as the branch and host footer items (wispComposerFooter.ts).
// The coordinator's chosen account is this host's role default (#119's `accounts/defaults/set`);
// see `IWispAccountsService.coordinatorChoice`.

registerAction2(class ComposerAccountAction extends Action2 {
	constructor() {
		super({
			id: WISP_COMPOSER_ACCOUNT_ACTION,
			title: localize2('wispComposer.account', "Account"),
			icon: Codicon.account,
			menu: { id: MenuId.ChatInputSecondary, group: 'navigation', order: 999, when: inProject },
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		await accessor.get(ICommandService).executeCommand(WISP_PICK_ACCOUNT_COMMAND);
	}
});

type AccountPickItem = IQuickPickItem & { readonly choice?: WispAccountChoice; readonly manage?: boolean };

async function pickAccount(accessor: ServicesAccessor): Promise<void> {
	const accountsService = accessor.get(IWispAccountsService);
	const quickInputService = accessor.get(IQuickInputService);
	const commandService = accessor.get(ICommandService);
	const notificationService = accessor.get(INotificationService);

	const clis = accountsService.clis.get().filter(cli => cli.signedIn);
	const keyAccounts = accountsService.keyAccounts.get();
	const items: Array<AccountPickItem | IQuickPickSeparator> = [];

	if (clis.length > 0) {
		items.push({ type: 'separator', label: localize('wispComposer.accountClis', "Detected CLIs") });
		for (const cli of clis) {
			items.push({ id: `cli:${cli.cli}`, label: cliAccountLabel(cli), choice: { kind: 'cli', cli: cli.cli } });
		}
	}
	if (keyAccounts.length > 0) {
		items.push({ type: 'separator', label: localize('wispComposer.accountKeys', "API Keys") });
		for (const account of keyAccounts) {
			items.push({ id: `key:${account.id}`, label: keyAccountLabel(account), choice: { kind: 'key', id: account.id } });
		}
	}
	if (items.length === 0) {
		items.push({ label: localize('wispComposer.accountNone', "No accounts yet"), description: localize('wispComposer.accountNoneDetail', "Add a key or sign in to a CLI in Manage Accounts") });
	}
	items.push({ type: 'separator' });
	items.push({ id: 'manage', label: localize('wispComposer.accountManage', "Manage Accounts..."), iconClass: ThemeIcon.asClassName(Codicon.settingsGear), manage: true });

	const picked = await quickInputService.pick(items, {
		title: localize('wispComposer.accountTitle', "Choose the Coordinator's Account"),
		placeHolder: localize('wispComposer.accountPlaceholder', "The account the coordinator uses for this project"),
	});
	if (!picked) {
		return;
	}
	if (picked.manage) {
		await commandService.executeCommand(WISP_SHOW_ACCOUNTS_COMMAND);
		return;
	}
	if (picked.choice) {
		try {
			await accountsService.setCoordinatorChoice(picked.choice);
		} catch (error) {
			notificationService.error(localize('wispComposer.accountSetFailed', "Couldn't set the coordinator's account: {0}", describeSetAccountError(error)));
		}
	}
}

function describeSetAccountError(error: unknown): string {
	if (error instanceof WispdError) {
		return error.message;
	}
	return error instanceof Error ? error.message : String(error);
}

registerAction2(class PickAccountAction extends Action2 {
	constructor() {
		super({
			id: WISP_PICK_ACCOUNT_COMMAND,
			title: localize2('wispComposer.pickAccount', "Choose Account..."),
			category,
			f1: true,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return pickAccount(accessor);
	}
});

/** The composer's account chip: the current account's label, and a chevron hinting it opens a picker. */
export class WispComposerAccountItem extends BaseActionViewItem {

	constructor(
		action: IAction,
		@IWispAccountsService private readonly accountsService: IWispAccountsService,
	) {
		super(undefined, action);
	}

	override render(container: HTMLElement): void {
		super.render(container);
		container.classList.add('wisp-composer-footer-item', 'wisp-composer-account');
		container.setAttribute('role', 'button');
		container.tabIndex = 0;
		const text = append(container, $('span.wisp-composer-footer-text', { 'aria-hidden': 'true' }));
		const chevron = append(container, $('span.wisp-composer-account-chevron', { 'aria-hidden': 'true' }));
		chevron.classList.add(...ThemeIcon.asClassNameArray(Codicon.chevronDown));

		this._register(autorun(reader => {
			const choice = this.accountsService.coordinatorChoice.read(reader);
			const clis = this.accountsService.clis.read(reader);
			const keyAccounts = this.accountsService.keyAccounts.read(reader);
			const label = resolveAccountLabel(choice, clis, keyAccounts);
			text.textContent = label;
			container.setAttribute('aria-label', localize('wispComposer.accountAria', "Account: {0}", label));
			container.title = localize('wispComposer.accountAria', "Account: {0}", label);
		}));
	}

	override focus(): void {
		this.element?.focus();
	}

	override isFocused(): boolean {
		return !!this.element && this.element.ownerDocument.activeElement === this.element;
	}
}

class WispComposerAccountContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispComposerAccount';

	constructor(
		@IActionViewItemService actionViewItemService: IActionViewItemService,
		@IInstantiationService instantiationService: IInstantiationService,
	) {
		super();
		this._register(actionViewItemService.register(MenuId.ChatInputSecondary, WISP_COMPOSER_ACCOUNT_ACTION, action => instantiationService.createInstance(WispComposerAccountItem, action)));
	}
}

registerWorkbenchContribution2(WispComposerAccountContribution.ID, WispComposerAccountContribution, WorkbenchPhase.BlockRestore);
