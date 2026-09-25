/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispAccounts.css';
import { $, append, clearNode, addDisposableListener, Dimension, EventType } from '../../../../base/browser/dom.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import Severity from '../../../../base/common/severity.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { IDialogService } from '../../../../platform/dialogs/common/dialogs.js';
import { EditorPaneDescriptor, IEditorPaneRegistry } from '../../../../workbench/browser/editor.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { IInstantiationService, ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { IHoverService } from '../../../../platform/hover/browser/hover.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService, IQuickPickItem } from '../../../../platform/quickinput/common/quickInput.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import { IStorageService } from '../../../../platform/storage/common/storage.js';
import { ITelemetryService } from '../../../../platform/telemetry/common/telemetry.js';
import { IThemeService } from '../../../../platform/theme/common/themeService.js';
import { generateUuidV7 } from '../../../../platform/wisp/common/uuidv7.js';
import { WispdError } from '../../../../platform/wisp/common/wispd.js';
import type { AccountId, AccountUsage, DetectedCli, KeyAccount, Provider } from '../../../../platform/wisp/common/wispProtocol.js';
import { EditorExtensions, IEditorFactoryRegistry, IEditorSerializer, IUntypedEditorInput } from '../../../../workbench/common/editor.js';
import { EditorInput } from '../../../../workbench/common/editor/editorInput.js';
import { EditorPane } from '../../../../workbench/browser/parts/editor/editorPane.js';
import { IEditorGroup } from '../../../../workbench/services/editor/common/editorGroupsService.js';
import { IEditorService } from '../../../../workbench/services/editor/common/editorService.js';
import {
	cliAccountLabel, formatLimitDetail, formatPeriodValue, formatUsedPercent,
	IWispAccountsService, keyAccountLabel, matchUsageAccount, providerLabel, windowLabel,
} from './wispAccounts.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_SHOW_ACCOUNTS_COMMAND = 'wisp.accounts.show';

const category = localize2('wisp', "Wisp");

export class WispAccountsEditorInput extends EditorInput {

	static readonly ID = 'workbench.editors.wisp.accounts';
	static readonly EDITOR_ID = 'workbench.editor.wisp.accounts';

	override readonly resource = undefined;

	override get typeId(): string {
		return WispAccountsEditorInput.ID;
	}

	override get editorId(): string {
		return WispAccountsEditorInput.EDITOR_ID;
	}

	override getName(): string {
		return localize('wispAccounts.editorName', "Accounts");
	}

	override getIcon(): ThemeIcon {
		return Codicon.account;
	}

	override matches(otherInput: EditorInput | IUntypedEditorInput): boolean {
		return otherInput instanceof WispAccountsEditorInput;
	}
}

class WispAccountsEditorInputSerializer implements IEditorSerializer {
	canSerialize(): boolean {
		return true;
	}
	serialize(): string {
		return '';
	}
	deserialize(instantiationService: IInstantiationService): EditorInput {
		return instantiationService.createInstance(WispAccountsEditorInput);
	}
}

/**
 * Customize > Accounts (docs/design/agents-window.md, issue #121): the CLIs wispd detects
 * (#114), key accounts (#117), and per-account usage (#120), as a plain editor pane opened next
 * to the coordinator, the way upstream opens Settings and Keyboard Shortcuts.
 */
export class WispAccountsEditor extends EditorPane {

	static readonly ID = WispAccountsEditorInput.EDITOR_ID;

	private root: HTMLElement | undefined;
	private firstFocusable: HTMLElement | undefined;
	private readonly clisRowStore = this._register(new DisposableStore());
	private readonly keysRowStore = this._register(new DisposableStore());

	constructor(
		group: IEditorGroup,
		@ITelemetryService telemetryService: ITelemetryService,
		@IThemeService themeService: IThemeService,
		@IStorageService storageService: IStorageService,
		@IInstantiationService private readonly instantiationService: IInstantiationService,
		@IWispAccountsService private readonly accountsService: IWispAccountsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IHoverService private readonly hoverService: IHoverService,
		@IDialogService private readonly dialogService: IDialogService,
		@INotificationService private readonly notificationService: INotificationService,
	) {
		super(WispAccountsEditor.ID, group, telemetryService, themeService, storageService);
	}

	protected override createEditor(parent: HTMLElement): void {
		const root = this.root = append(parent, $('.wisp-accounts', { role: 'region', 'aria-label': localize('wispAccounts.region', "Accounts") }));
		append(root, $('h1.wisp-accounts-title', undefined, localize('wispAccounts.title', "Accounts")));

		this.accountsService.reload();

		this.renderClisSection(root);
		this.renderKeysSection(root);
		this.renderUsageSection(root);
	}

	private renderClisSection(root: HTMLElement): void {
		const { section, header } = this.renderSection(root, localize('wispAccounts.clis', "Detected CLIs"));
		const refreshLabel = localize('wispAccounts.refresh', "Refresh");
		const refresh = append(header, $<HTMLButtonElement>('button.wisp-accounts-icon-button', { type: 'button', 'aria-label': refreshLabel }));
		this.firstFocusable = refresh;
		append(refresh, $(`span${ThemeIcon.asCSSSelector(Codicon.refresh)}`, { 'aria-hidden': 'true' }));
		this._register(this.hoverService.setupDelayedHover(refresh, { content: refreshLabel }));
		let refreshEnabled = false;
		this._register(addDisposableListener(refresh, EventType.CLICK, () => {
			if (refreshEnabled) {
				this.accountsService.refreshClis();
			}
		}));

		const list = append(section, $('ul.wisp-accounts-list'));
		this._register(autorun(reader => {
			const connected = this.hostStatusService.status.read(reader).kind === 'connected';
			const supported = this.accountsService.clisSupported.read(reader);
			const state = this.accountsService.clisState.read(reader);
			const clis = this.accountsService.clis.read(reader);
			refreshEnabled = connected && supported;
			setDisabled(refresh, !refreshEnabled);

			this.clisRowStore.clear();
			clearNode(list);
			if (!connected) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.clisNoHost', "Connect to a host to see its detected CLIs.")));
				return;
			}
			if (!supported) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.clisUnsupported', "This wispd doesn't report detected CLIs yet. Update it to see them here.")));
				return;
			}
			if (state.kind === 'loading' && clis.length === 0) {
				list.setAttribute('aria-busy', 'true');
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.loading', "Loading...")));
				return;
			}
			list.removeAttribute('aria-busy');
			if (state.kind === 'failed') {
				append(list, $('li.wisp-accounts-empty', undefined, state.message));
				return;
			}
			if (clis.length === 0) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.noClis', "wisp didn't detect any agent CLIs on this host.")));
				return;
			}
			for (const cli of clis) {
				list.appendChild(this.renderCliRow(cli));
			}
		}));
	}

	private renderCliRow(cli: DetectedCli): HTMLElement {
		const item = $('li.wisp-accounts-row');
		const icon = append(item, $('span.wisp-accounts-row-icon', { 'aria-hidden': 'true' }));
		icon.classList.add(...ThemeIcon.asClassNameArray(Codicon.terminal));
		const text = append(item, $('.wisp-accounts-row-text'));
		append(text, $('span.wisp-accounts-row-title', undefined, cliAccountLabel(cli)));
		append(text, $('span.wisp-accounts-row-detail', undefined, describeCliState(cli)));

		const signInLabel = localize('wispAccounts.signIn', "Sign in");
		const signIn = append(item, $<HTMLButtonElement>('button.wisp-accounts-link-button', { type: 'button' }, signInLabel));
		const reason = localize('wispAccounts.signInUnavailable', "Signing in from wisp isn't available yet.");
		setDisabled(signIn, true, reason);
		this.clisRowStore.add(this.hoverService.setupDelayedHover(signIn, { content: reason }));

		item.setAttribute('aria-label', `${cliAccountLabel(cli)}, ${describeCliState(cli)}`);
		return item;
	}

	private renderKeysSection(root: HTMLElement): void {
		const { section, header } = this.renderSection(root, localize('wispAccounts.keys', "API Keys"));
		const addLabel = localize('wispAccounts.addKey', "Add Key...");
		const add = append(header, $<HTMLButtonElement>('button.wisp-accounts-link-button', { type: 'button' }, addLabel));
		let addEnabled = false;
		this._register(addDisposableListener(add, EventType.CLICK, () => {
			if (addEnabled) {
				this.instantiationService.createInstance(WispAddKeyFlow).run();
			}
		}));

		const list = append(section, $('ul.wisp-accounts-list'));
		this._register(autorun(reader => {
			const connected = this.hostStatusService.status.read(reader).kind === 'connected';
			const state = this.accountsService.keyAccountsState.read(reader);
			const accounts = this.accountsService.keyAccounts.read(reader);
			addEnabled = connected;
			setDisabled(add, !addEnabled);

			this.keysRowStore.clear();
			clearNode(list);
			if (!connected) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.keysNoHost', "Connect to a host to manage its API keys.")));
				return;
			}
			if (state.kind === 'loading' && accounts.length === 0) {
				list.setAttribute('aria-busy', 'true');
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.loading', "Loading...")));
				return;
			}
			list.removeAttribute('aria-busy');
			if (state.kind === 'failed') {
				append(list, $('li.wisp-accounts-empty', undefined, state.message));
				return;
			}
			if (accounts.length === 0) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.noKeys', "No API keys added yet.")));
				return;
			}
			for (const account of accounts) {
				list.appendChild(this.renderKeyRow(account));
			}
		}));
	}

	private renderKeyRow(account: KeyAccount): HTMLElement {
		const item = $('li.wisp-accounts-row');
		const icon = append(item, $('span.wisp-accounts-row-icon', { 'aria-hidden': 'true' }));
		icon.classList.add(...ThemeIcon.asClassNameArray(Codicon.key));
		const text = append(item, $('.wisp-accounts-row-text'));
		append(text, $('span.wisp-accounts-row-title', undefined, keyAccountLabel(account)));
		append(text, $('span.wisp-accounts-row-detail', undefined, account.maskedKey));

		const removeLabel = localize('wispAccounts.removeKey', "Remove {0}", keyAccountLabel(account));
		const remove = append(item, $<HTMLButtonElement>('button.wisp-accounts-icon-button', { type: 'button', 'aria-label': removeLabel }));
		append(remove, $(`span${ThemeIcon.asCSSSelector(Codicon.trash)}`, { 'aria-hidden': 'true' }));
		this.keysRowStore.add(this.hoverService.setupDelayedHover(remove, { content: removeLabel }));
		this.keysRowStore.add(addDisposableListener(remove, EventType.CLICK, () => this.removeKey(account)));

		item.setAttribute('aria-label', `${keyAccountLabel(account)}, ${account.maskedKey}`);
		return item;
	}

	private async removeKey(account: KeyAccount): Promise<void> {
		const { confirmed } = await this.dialogService.confirm({
			type: Severity.Warning,
			message: localize('wispAccounts.removeConfirmTitle', "Remove {0}?", keyAccountLabel(account)),
			detail: localize('wispAccounts.removeConfirmDetail', "wisp forgets this key. Tasks routed to it fall back to their role's other accounts."),
			primaryButton: localize('wispAccounts.removeConfirmButton', "Remove"),
		});
		if (!confirmed) {
			return;
		}
		try {
			await this.accountsService.removeKey(account.id);
		} catch (error) {
			this.notificationService.error(localize('wispAccounts.removeFailed', "Couldn't remove {0}: {1}", keyAccountLabel(account), describeError(error)));
		}
	}

	private renderUsageSection(root: HTMLElement): void {
		const { section } = this.renderSection(root, localize('wispAccounts.usage', "Usage"));
		const list = append(section, $('ul.wisp-accounts-list'));
		this._register(autorun(reader => {
			const connected = this.hostStatusService.status.read(reader).kind === 'connected';
			const state = this.accountsService.usageState.read(reader);
			const usage = this.accountsService.usage.read(reader);
			const clis = this.accountsService.clis.read(reader);
			const keyAccounts = this.accountsService.keyAccounts.read(reader);

			clearNode(list);
			if (!connected) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.usageNoHost', "Connect to a host to see account usage.")));
				return;
			}
			if (state.kind === 'loading' && usage.length === 0) {
				list.setAttribute('aria-busy', 'true');
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.loading', "Loading...")));
				return;
			}
			list.removeAttribute('aria-busy');
			if (state.kind === 'failed') {
				append(list, $('li.wisp-accounts-empty', undefined, state.message));
				return;
			}
			if (usage.length === 0) {
				append(list, $('li.wisp-accounts-empty', undefined, localize('wispAccounts.noUsage', "No usage recorded yet.")));
				return;
			}
			for (const entry of usage) {
				list.appendChild(this.renderUsageRow(entry, clis, keyAccounts));
			}
		}));
	}

	private renderUsageRow(entry: AccountUsage, clis: readonly DetectedCli[], keyAccounts: readonly KeyAccount[]): HTMLElement {
		const matched = matchUsageAccount(entry, clis, keyAccounts);
		const label = matched.kind === 'cli' ? cliAccountLabel(matched.cli)
			: matched.kind === 'key' ? keyAccountLabel(matched.account)
				: matched.accountId;

		const item = $('li.wisp-accounts-usage-row');
		append(item, $('span.wisp-accounts-row-title', undefined, label));

		const periods = append(item, $('.wisp-accounts-usage-periods'));
		append(periods, this.renderPeriod(localize('wispAccounts.today', "Today"), entry.today));
		append(periods, this.renderPeriod(localize('wispAccounts.week', "This week"), entry.week));

		if (entry.limits.length > 0) {
			const limits = append(item, $('ul.wisp-accounts-limits'));
			for (const limit of entry.limits) {
				const row = append(limits, $('li.wisp-accounts-limit'));
				append(row, $('span.wisp-accounts-limit-name', undefined, windowLabel(limit.window)));
				const bar = append(row, $('.wisp-accounts-limit-bar', {
					role: 'progressbar',
					'aria-valuemin': '0',
					'aria-valuemax': '100',
					...(typeof limit.usedPercent === 'number' ? { 'aria-valuenow': String(Math.round(limit.usedPercent)) } : {}),
					'aria-label': `${windowLabel(limit.window)}, ${formatUsedPercent(limit.usedPercent)}`,
				}));
				append(bar, $('.wisp-accounts-limit-fill', { style: `width: ${typeof limit.usedPercent === 'number' ? Math.max(0, Math.min(100, limit.usedPercent)) : 0}%` }));
				append(row, $('span.wisp-accounts-limit-detail', { 'aria-hidden': 'true' }, formatLimitDetail(limit)));
			}
		}
		return item;
	}

	private renderPeriod(label: string, period: { readonly inputTokens: number; readonly outputTokens: number; readonly cacheReadTokens: number; readonly cacheWriteTokens: number; readonly costUsdMicros?: number | null }): HTMLElement {
		const row = $('.wisp-accounts-period');
		append(row, $('span.wisp-accounts-period-label', undefined, label));
		append(row, $('span.wisp-accounts-period-value', undefined, formatPeriodValue(period)));
		return row;
	}

	private renderSection(root: HTMLElement, title: string): { section: HTMLElement; header: HTMLElement } {
		const section = append(root, $('section.wisp-accounts-section'));
		const header = append(section, $('.wisp-accounts-section-header'));
		append(header, $('h2.wisp-accounts-section-title', undefined, title));
		return { section, header };
	}

	override layout(dimension: Dimension): void {
		if (this.root) {
			this.root.style.height = `${dimension.height}px`;
			this.root.style.width = `${dimension.width}px`;
		}
	}

	override focus(): void {
		super.focus();
		this.firstFocusable?.focus();
	}
}

/** Toggles a button's disabled state through `aria-disabled`, not the `disabled` attribute, so it keeps focus and can name why. */
export function setDisabled(button: HTMLButtonElement, disabled: boolean, reason?: string): void {
	button.classList.toggle('disabled', disabled);
	if (disabled) {
		button.setAttribute('aria-disabled', 'true');
		if (reason) {
			button.setAttribute('aria-description', reason);
		}
	} else {
		button.removeAttribute('aria-disabled');
		button.removeAttribute('aria-description');
	}
}

export function describeCliState(cli: DetectedCli): string {
	if (!cli.installed) {
		return localize('wispAccounts.notInstalled', "Not installed");
	}
	if (cli.signedIn === undefined) {
		return localize('wispAccounts.signedInUnknown', "Installed; sign-in state unknown");
	}
	return cli.signedIn ? localize('wispAccounts.signedIn', "Signed in") : localize('wispAccounts.notSignedIn', "Not signed in");
}

function describeError(error: unknown): string {
	if (error instanceof WispdError) {
		return error.message;
	}
	return toErrorMessage(error);
}

interface IAskOptions {
	readonly title: string;
	readonly prompt: string;
	readonly placeholder?: string;
	readonly password?: boolean;
	readonly validate: (text: string) => string | undefined;
}

const PROVIDERS: ReadonlyArray<{ readonly provider: Provider; readonly label: string }> = [
	{ provider: 'anthropic', label: providerLabel('anthropic') },
	{ provider: 'openai', label: providerLabel('openai') },
	{ provider: 'cursor', label: providerLabel('cursor') },
];

/** Adds a key account: a provider, a label, then the key itself, sent once (0004, #117). */
class WispAddKeyFlow {

	constructor(
		@IQuickInputService private readonly quickInputService: IQuickInputService,
		@IWispAccountsService private readonly accountsService: IWispAccountsService,
		@INotificationService private readonly notificationService: INotificationService,
	) { }

	async run(): Promise<KeyAccount | undefined> {
		const provider = await this.pickProvider();
		if (!provider) {
			return undefined;
		}
		const label = await this.ask({
			title: localize('wispAccounts.keyLabelTitle', "Add {0} Key: Label", providerLabel(provider)),
			prompt: localize('wispAccounts.keyLabelPrompt', "A label to tell this key apart, shown in wisp"),
			validate: text => text.length === 0 ? localize('wispAccounts.keyLabelEmpty', "Enter a label.") : undefined,
		});
		if (label === undefined) {
			return undefined;
		}
		const key = await this.ask({
			title: localize('wispAccounts.keyValueTitle', "Add {0} Key: Key", providerLabel(provider)),
			prompt: localize('wispAccounts.keyValuePrompt', "The key. wisp sends it once and never shows it again."),
			password: true,
			validate: text => text.length === 0 ? localize('wispAccounts.keyValueEmpty', "Enter a key.") : undefined,
		});
		if (key === undefined) {
			return undefined;
		}
		const id: AccountId = generateUuidV7();
		try {
			return await this.accountsService.addKey({ id, provider, label, key });
		} catch (error) {
			this.notificationService.error(localize('wispAccounts.addKeyFailed', "Couldn't add the key: {0}", describeError(error)));
			return undefined;
		}
	}

	private async pickProvider(): Promise<Provider | undefined> {
		const picked = await this.quickInputService.pick(
			PROVIDERS.map((entry): IQuickPickItem & { provider: Provider } => ({ id: entry.provider, label: entry.label, provider: entry.provider })),
			{ title: localize('wispAccounts.pickProviderTitle', "Add Key: Choose a Vendor"), placeHolder: localize('wispAccounts.pickProviderPlaceholder', "Who is this key for?") },
		);
		return picked?.provider;
	}

	private ask(options: IAskOptions): Promise<string | undefined> {
		const store = new DisposableStore();
		const box = store.add(this.quickInputService.createInputBox());
		box.title = options.title;
		box.prompt = options.prompt;
		box.placeholder = options.placeholder;
		box.password = options.password ?? false;
		box.ignoreFocusOut = true;
		return new Promise<string | undefined>(resolve => {
			let accepted: string | undefined;
			store.add(box.onDidChangeValue(() => { box.validationMessage = undefined; box.severity = Severity.Ignore; }));
			store.add(box.onDidAccept(() => {
				const text = options.password ? box.value : box.value.trim();
				const problem = options.validate(text);
				if (problem) {
					box.validationMessage = problem;
					box.severity = Severity.Error;
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

async function showAccounts(accessor: ServicesAccessor): Promise<void> {
	const editorService = accessor.get(IEditorService);
	const instantiationService = accessor.get(IInstantiationService);
	await editorService.openEditor(instantiationService.createInstance(WispAccountsEditorInput));
}

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: WISP_SHOW_ACCOUNTS_COMMAND,
			title: localize2('wispAccounts.show', "Show Accounts"),
			category,
			f1: true,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return showAccounts(accessor);
	}
});

Registry.as<IEditorPaneRegistry>(EditorExtensions.EditorPane).registerEditorPane(
	EditorPaneDescriptor.create(
		WispAccountsEditor,
		WispAccountsEditor.ID,
		localize('wispAccounts.editorName', "Accounts"),
	),
	[new SyncDescriptor(WispAccountsEditorInput)],
);

Registry.as<IEditorFactoryRegistry>(EditorExtensions.EditorFactory).registerEditorSerializer(WispAccountsEditorInput.ID, WispAccountsEditorInputSerializer);
