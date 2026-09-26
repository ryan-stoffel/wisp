/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { fromNow } from '../../../../base/common/date.js';
import { Disposable } from '../../../../base/common/lifecycle.js';
import { IObservable, observableValue, transaction } from '../../../../base/common/observable.js';
import { localize } from '../../../../nls.js';
import { createDecorator } from '../../../../platform/instantiation/common/instantiation.js';
import { ILogService } from '../../../../platform/log/common/log.js';
import { IStorageService, StorageScope } from '../../../../platform/storage/common/storage.js';
import { IWispdService, WispdError, WispdState } from '../../../../platform/wisp/common/wispd.js';
import type { AccountChoice, AccountId, AccountUsage, CliKind, DetectedCli, KeyAccount, Provider, RawKey } from '../../../../platform/wisp/common/wispProtocol.js';

export const IWispAccountsService = createDecorator<IWispAccountsService>('wispAccountsService');

/** `accounts/list` and `accounts/refresh` answer only when wispd reports this capability. */
export const AGENT_CLIS_CAPABILITY = 'agentClis';

/** A detected CLI's subscription, or a key account: the composer's account picker chooses one. */
export type WispAccountChoice =
	| { readonly kind: 'cli'; readonly cli: CliKind }
	| { readonly kind: 'key'; readonly id: AccountId };

/** #121's local-only storage key. Read once to migrate into wispd's role default, then deleted;
 * see {@link WispAccountsService.migrateLocalChoice}. */
const COORDINATOR_CHOICE_KEY = 'wisp.accounts.coordinatorChoice';

/**
 * Where one of the Accounts view's lists stands.
 *
 * - `idle`: not loaded yet, for example because no host is connected.
 * - `loading`: the request is on its way.
 * - `ready`: the list is current.
 * - `failed`: wispd answered with an error.
 */
export type WispAccountsLoadState =
	| { readonly kind: 'idle' }
	| { readonly kind: 'loading' }
	| { readonly kind: 'ready' }
	| { readonly kind: 'failed'; readonly message: string };

/**
 * The Accounts view's data, read from the editor's wispd connection on demand (there are no
 * account or usage events yet, decision record 0007's `events/event` covers only projects): the
 * view calls {@link IWispAccountsService.reload} when it opens, and a reconnect while it is open
 * reloads automatically. wispd's account registry (#114, #117, #118) is still forming, so a key
 * account and a detected CLI are unrelated lists; {@link matchUsageAccount} is what ties a
 * `usage/get` entry back to one of them.
 */
export interface IWispAccountsService {
	readonly _serviceBrand: undefined;

	/** The vendor CLIs wispd detects (#114). Empty, and {@link clisState} stays `idle`, when
	 * {@link clisSupported} is `false`. */
	readonly clis: IObservable<readonly DetectedCli[]>;
	readonly clisState: IObservable<WispAccountsLoadState>;
	/** Whether the connected wispd reports the `agentClis` capability. */
	readonly clisSupported: IObservable<boolean>;

	/** API key accounts (0004's fallback), oldest first, with their keys masked (#117). */
	readonly keyAccounts: IObservable<readonly KeyAccount[]>;
	readonly keyAccountsState: IObservable<WispAccountsLoadState>;

	/** Every account wispd has recorded usage or limits for (#120). */
	readonly usage: IObservable<readonly AccountUsage[]>;
	readonly usageState: IObservable<WispAccountsLoadState>;

	/** Loads everything the connection currently supports. Call when the Accounts view opens. */
	reload(): void;

	/** Probes the CLIs again instead of answering from wispd's cache. */
	refreshClis(): Promise<void>;

	/**
	 * Adds a key account. `id` should be generated once with `generateUuidV7`; resending the same
	 * `id` after a lost connection returns the account `accounts/keys/add` already made.
	 */
	addKey(params: { readonly id: AccountId; readonly provider: Provider; readonly label: string; readonly key: RawKey }): Promise<KeyAccount>;

	/** Removes a key account. Fails with a `WispdError` whose `kind` is `accountNotFound` if it is already gone. */
	removeKey(id: AccountId): Promise<void>;

	/**
	 * This host's default account for the coordinator role (#119's `accounts/defaults/get`),
	 * absent when none is set. Loaded by {@link reload} and kept current across a reconnect.
	 */
	readonly coordinatorChoice: IObservable<WispAccountChoice | undefined>;
	readonly coordinatorChoiceState: IObservable<WispAccountsLoadState>;

	/**
	 * Sets or clears the coordinator's default account with `accounts/defaults/set`. Rejects with
	 * the `WispdError` wispd answered with (`invalidParams` names the offending account or
	 * backend, per decision record 0012) after reverting {@link coordinatorChoice} to its previous
	 * value; the caller is responsible for telling the user.
	 */
	setCoordinatorChoice(choice: WispAccountChoice | undefined): Promise<void>;
}

export class WispAccountsService extends Disposable implements IWispAccountsService {
	declare readonly _serviceBrand: undefined;

	private readonly _clis = observableValue<readonly DetectedCli[]>(this, []);
	readonly clis: IObservable<readonly DetectedCli[]> = this._clis;
	private readonly _clisState = observableValue<WispAccountsLoadState>(this, { kind: 'idle' });
	readonly clisState: IObservable<WispAccountsLoadState> = this._clisState;
	private readonly _clisSupported = observableValue<boolean>(this, false);
	readonly clisSupported: IObservable<boolean> = this._clisSupported;

	private readonly _keyAccounts = observableValue<readonly KeyAccount[]>(this, []);
	readonly keyAccounts: IObservable<readonly KeyAccount[]> = this._keyAccounts;
	private readonly _keyAccountsState = observableValue<WispAccountsLoadState>(this, { kind: 'idle' });
	readonly keyAccountsState: IObservable<WispAccountsLoadState> = this._keyAccountsState;

	private readonly _usage = observableValue<readonly AccountUsage[]>(this, []);
	readonly usage: IObservable<readonly AccountUsage[]> = this._usage;
	private readonly _usageState = observableValue<WispAccountsLoadState>(this, { kind: 'idle' });
	readonly usageState: IObservable<WispAccountsLoadState> = this._usageState;

	/** Whether `reload` has ever been asked for, so a reconnect knows to load again. */
	private wanted = false;
	private connection: WispdState | undefined;

	private readonly _coordinatorChoice = observableValue<WispAccountChoice | undefined>(this, undefined);
	readonly coordinatorChoice: IObservable<WispAccountChoice | undefined> = this._coordinatorChoice;
	private readonly _coordinatorChoiceState = observableValue<WispAccountsLoadState>(this, { kind: 'idle' });
	readonly coordinatorChoiceState: IObservable<WispAccountsLoadState> = this._coordinatorChoiceState;

	constructor(
		@IWispdService private readonly wispdService: IWispdService,
		@ILogService private readonly logService: ILogService,
		@IStorageService private readonly storageService: IStorageService,
	) {
		super();
		this._register(wispdService.onDidChangeState(state => this.onState(state)));
		wispdService.getState().then(state => this.onState(state), () => { /* the shared process is gone; the window is closing */ });
	}

	async setCoordinatorChoice(choice: WispAccountChoice | undefined): Promise<void> {
		const previous = this._coordinatorChoice.get();
		this._coordinatorChoice.set(choice, undefined);
		try {
			const result = await this.wispdService.request('accounts/defaults/set', {
				role: 'coordinator',
				account: choice ? toAccountChoice(choice) : undefined,
			});
			this._coordinatorChoice.set(fromAccountChoice(result.coordinator), undefined);
		} catch (error) {
			this._coordinatorChoice.set(previous, undefined);
			throw error;
		}
	}

	reload(): void {
		this.wanted = true;
		if (this.connection?.kind !== 'connected') {
			return;
		}
		this._clisSupported.set(AGENT_CLIS_CAPABILITY in this.connection.capabilities, undefined);
		if (this.clisSupported.get()) {
			this.listClis();
		} else {
			transaction(tx => {
				this._clis.set([], tx);
				this._clisState.set({ kind: 'idle' }, tx);
			});
		}
		this.listKeyAccounts();
		this.listUsage();
		this.loadCoordinatorChoice();
	}

	async refreshClis(): Promise<void> {
		if (this.connection?.kind !== 'connected' || !this.clisSupported.get()) {
			return;
		}
		this._clisState.set({ kind: 'loading' }, undefined);
		try {
			const { clis } = await this.wispdService.request('accounts/refresh', {});
			this._clis.set(clis, undefined);
			this._clisState.set({ kind: 'ready' }, undefined);
		} catch (error) {
			this.logService.error('[wisp] accounts/refresh failed', error);
			this._clisState.set({ kind: 'failed', message: describeFailure(error) }, undefined);
		}
	}

	async addKey(params: { readonly id: AccountId; readonly provider: Provider; readonly label: string; readonly key: RawKey }): Promise<KeyAccount> {
		const { account } = await this.wispdService.request('accounts/keys/add', params);
		const accounts = this._keyAccounts.get();
		if (!accounts.some(existing => existing.id === account.id)) {
			this._keyAccounts.set([...accounts, account], undefined);
		}
		return account;
	}

	async removeKey(id: AccountId): Promise<void> {
		await this.wispdService.request('accounts/keys/remove', { id });
		this._keyAccounts.set(this._keyAccounts.get().filter(account => account.id !== id), undefined);
	}

	private onState(state: WispdState): void {
		const previous = this.connection;
		this.connection = state;
		if (state.kind === 'connected' && (previous === undefined || previous.kind !== 'connected' || previous.command !== state.command) && this.wanted) {
			this.reload();
		}
	}

	private async listClis(): Promise<void> {
		this._clisState.set({ kind: 'loading' }, undefined);
		try {
			const { clis } = await this.wispdService.request('accounts/list', {});
			this._clis.set(clis, undefined);
			this._clisState.set({ kind: 'ready' }, undefined);
		} catch (error) {
			this.logService.error('[wisp] accounts/list failed', error);
			this._clisState.set({ kind: 'failed', message: describeFailure(error) }, undefined);
		}
	}

	private async listKeyAccounts(): Promise<void> {
		this._keyAccountsState.set({ kind: 'loading' }, undefined);
		try {
			const { accounts } = await this.wispdService.request('accounts/keys/list', {});
			this._keyAccounts.set(accounts, undefined);
			this._keyAccountsState.set({ kind: 'ready' }, undefined);
		} catch (error) {
			this.logService.error('[wisp] accounts/keys/list failed', error);
			this._keyAccountsState.set({ kind: 'failed', message: describeFailure(error) }, undefined);
		}
	}

	private async listUsage(): Promise<void> {
		this._usageState.set({ kind: 'loading' }, undefined);
		try {
			const { accounts } = await this.wispdService.request('usage/get', {});
			this._usage.set(accounts, undefined);
			this._usageState.set({ kind: 'ready' }, undefined);
		} catch (error) {
			this.logService.error('[wisp] usage/get failed', error);
			this._usageState.set({ kind: 'failed', message: describeFailure(error) }, undefined);
		}
	}

	private async loadCoordinatorChoice(): Promise<void> {
		this._coordinatorChoiceState.set({ kind: 'loading' }, undefined);
		try {
			const result = await this.wispdService.request('accounts/defaults/get', {});
			const existing = fromAccountChoice(result.coordinator);
			const local = this.takeLocalChoice();
			const coordinator = existing ?? (local ? await this.migrateLocalChoice(local) : undefined);
			this._coordinatorChoice.set(coordinator, undefined);
			this._coordinatorChoiceState.set({ kind: 'ready' }, undefined);
		} catch (error) {
			this.logService.error('[wisp] accounts/defaults/get failed', error);
			this._coordinatorChoiceState.set({ kind: 'failed', message: describeFailure(error) }, undefined);
		}
	}

	/** Reads #121's local-only choice and deletes it, so this runs at most once ever: the first
	 * `accounts/defaults/get` that succeeds, whether or not wispd already had a coordinator
	 * default (in which case the local value is just discarded, never overwriting it). */
	private takeLocalChoice(): WispAccountChoice | undefined {
		const raw = this.storageService.get(COORDINATOR_CHOICE_KEY, StorageScope.APPLICATION);
		if (raw === undefined) {
			return undefined;
		}
		this.storageService.remove(COORDINATOR_CHOICE_KEY, StorageScope.APPLICATION);
		try {
			const parsed: unknown = JSON.parse(raw);
			return isAccountChoice(parsed) ? parsed : undefined;
		} catch {
			return undefined;
		}
	}

	/** Sends #121's locally stored choice to wispd once, only when wispd has no coordinator
	 * default yet. A failure (for example a key account since removed) just drops it: the local
	 * copy is already gone by the time this runs, so there is nothing left to retry. */
	private async migrateLocalChoice(choice: WispAccountChoice): Promise<WispAccountChoice | undefined> {
		try {
			const result = await this.wispdService.request('accounts/defaults/set', {
				role: 'coordinator',
				account: toAccountChoice(choice),
			});
			return fromAccountChoice(result.coordinator);
		} catch (error) {
			this.logService.error('[wisp] failed to migrate the locally stored coordinator account choice', error);
			return undefined;
		}
	}
}

const KNOWN_CLI_KINDS: readonly CliKind[] = ['claude', 'codex', 'cursor'];

function isCliKind(value: string): value is CliKind {
	return (KNOWN_CLI_KINDS as readonly string[]).includes(value);
}

/** {@link WispAccountChoice} as wispd's `accounts/defaults/set` wants it. */
function toAccountChoice(choice: WispAccountChoice): AccountChoice {
	return choice.kind === 'cli' ? { kind: 'subscription', backend: choice.cli } : { kind: 'key', id: choice.id };
}

/**
 * wispd's `AccountChoice` as the composer's picker understands it. A newer wispd may send a
 * `kind`, or a subscription `backend`, this version does not know: both come back as `undefined`
 * rather than a choice the picker cannot render, mirroring `AccountChoice`'s own doc comment
 * ("treat that as absent rather than end a switch... in an exhaustiveness assertion").
 */
function fromAccountChoice(choice: AccountChoice | undefined): WispAccountChoice | undefined {
	if (!choice) {
		return undefined;
	}
	if (choice.kind === 'subscription') {
		return isCliKind(choice.backend) ? { kind: 'cli', cli: choice.backend } : undefined;
	}
	if (choice.kind === 'key') {
		return { kind: 'key', id: choice.id };
	}
	return undefined;
}

function isAccountChoice(value: unknown): value is WispAccountChoice {
	if (typeof value !== 'object' || value === null) {
		return false;
	}
	const candidate = value as { kind?: unknown; cli?: unknown; id?: unknown };
	return (candidate.kind === 'cli' && typeof candidate.cli === 'string')
		|| (candidate.kind === 'key' && typeof candidate.id === 'string');
}

function describeFailure(error: unknown): string {
	if (error instanceof WispdError) {
		return error.message;
	}
	return toErrorMessage(error);
}

/** A detected CLI's vendor name, as the Accounts view and the composer's account picker show it. */
export function cliLabel(cli: CliKind | string): string {
	switch (cli) {
		case 'claude': return localize('wispAccounts.cliClaude', "Claude Code");
		case 'codex': return localize('wispAccounts.cliCodex', "Codex");
		case 'cursor': return localize('wispAccounts.cliCursor', "Cursor");
		default: return cli;
	}
}

/** An API key account's vendor name. A newer wispd may send a `provider` not listed here (0004). */
export function providerLabel(provider: Provider | string): string {
	switch (provider) {
		case 'anthropic': return localize('wispAccounts.providerAnthropic', "Anthropic");
		case 'openai': return localize('wispAccounts.providerOpenAI', "OpenAI");
		case 'cursor': return localize('wispAccounts.providerCursor', "Cursor");
		default: return provider;
	}
}

/** One line for a detected CLI: its plan when known, "Claude Code" alone otherwise. */
export function cliAccountLabel(cli: DetectedCli): string {
	return cli.plan ? localize('wispAccounts.cliWithPlan', "{0}: {1}", cliLabel(cli.cli), cli.plan) : cliLabel(cli.cli);
}

/** One line for a key account: its vendor and the label the user chose. */
export function keyAccountLabel(account: KeyAccount): string {
	return localize('wispAccounts.keyLabel', "{0}: {1}", providerLabel(account.provider), account.label);
}

export type MatchedUsageAccount =
	| { readonly kind: 'cli'; readonly cli: DetectedCli }
	| { readonly kind: 'key'; readonly account: KeyAccount }
	| { readonly kind: 'unknown'; readonly accountId: string };

/**
 * Ties a `usage/get` entry back to a detected CLI or a key account. wispd has no account registry
 * yet (the type's own comment), so a CLI's usage is keyed by its `CliKind` string and a key
 * account's by its id; neither match falls back to `unknown`, shown with its raw id.
 */
export function matchUsageAccount(usage: AccountUsage, clis: readonly DetectedCli[], keyAccounts: readonly KeyAccount[]): MatchedUsageAccount {
	const cli = clis.find(candidate => candidate.cli === usage.accountId);
	if (cli) {
		return { kind: 'cli', cli };
	}
	const account = keyAccounts.find(candidate => candidate.id === usage.accountId);
	if (account) {
		return { kind: 'key', account };
	}
	return { kind: 'unknown', accountId: usage.accountId };
}

const NOT_REPORTED = localize('wispAccounts.notReported', "Not reported");

/** A period's token total, formatted with grouping, such as "12,345". */
export function formatTokenTotal(period: { readonly inputTokens: number; readonly outputTokens: number; readonly cacheReadTokens: number; readonly cacheWriteTokens: number }): string {
	const total = period.inputTokens + period.outputTokens + period.cacheReadTokens + period.cacheWriteTokens;
	return total.toLocaleString();
}

/** A period's cost, or "Not reported" when the vendor never sends one (0004: Codex and Cursor). */
export function formatCost(costUsdMicros: number | null | undefined): string {
	if (costUsdMicros === undefined || costUsdMicros === null) {
		return NOT_REPORTED;
	}
	return `$${(costUsdMicros / 1_000_000).toFixed(2)}`;
}

/** A limit window's used percent, or "Not reported" when the vendor doesn't send one. */
export function formatUsedPercent(usedPercent: number | null | undefined): string {
	if (usedPercent === undefined || usedPercent === null) {
		return NOT_REPORTED;
	}
	return localize('wispAccounts.usedPercent', "{0}% used", Math.round(usedPercent));
}

/** A limit window's reset time, relative to now, or "Not reported" when the vendor doesn't send one. */
export function formatResetTime(resetsAt: string | null | undefined): string {
	if (!resetsAt) {
		return NOT_REPORTED;
	}
	const parsed = Date.parse(resetsAt);
	if (Number.isNaN(parsed)) {
		return NOT_REPORTED;
	}
	return localize('wispAccounts.resets', "Resets {0}", fromNow(parsed, false));
}

/** A usage period's row in the Accounts view, such as "12,345 tokens · $0.42" or "0 tokens · Not reported". */
export function formatPeriodValue(period: { readonly inputTokens: number; readonly outputTokens: number; readonly cacheReadTokens: number; readonly cacheWriteTokens: number; readonly costUsdMicros?: number | null }): string {
	return localize('wispAccounts.periodValue', "{0} tokens · {1}", formatTokenTotal(period), formatCost(period.costUsdMicros));
}

/** A limit window's detail line, such as "38% used · Resets in 2 hr". */
export function formatLimitDetail(limit: { readonly usedPercent?: number | null; readonly resetsAt?: string | null }): string {
	return localize('wispAccounts.limitDetail', "{0} · {1}", formatUsedPercent(limit.usedPercent), formatResetTime(limit.resetsAt));
}

const NO_ACCOUNT = localize('wispAccounts.noAccount', "No account");

/**
 * The composer's account picker's current label (docs/design/agents-window.md's `Claude Code:
 * Max`): the remembered choice when it is still usable, else the first signed-in CLI, else the
 * first key account, else "No account".
 */
export function resolveAccountLabel(choice: WispAccountChoice | undefined, clis: readonly DetectedCli[], keyAccounts: readonly KeyAccount[]): string {
	if (choice?.kind === 'cli') {
		const cli = clis.find(candidate => candidate.cli === choice.cli && candidate.signedIn);
		if (cli) {
			return cliAccountLabel(cli);
		}
	} else if (choice?.kind === 'key') {
		const account = keyAccounts.find(candidate => candidate.id === choice.id);
		if (account) {
			return keyAccountLabel(account);
		}
	}
	const firstCli = clis.find(candidate => candidate.signedIn);
	if (firstCli) {
		return cliAccountLabel(firstCli);
	}
	if (keyAccounts.length > 0) {
		return keyAccountLabel(keyAccounts[0]);
	}
	return NO_ACCOUNT;
}

/** The vendor's name for a limit window, such as `five_hour` or `seven_day`, in words. */
export function windowLabel(window: string): string {
	switch (window) {
		case 'five_hour': return localize('wispAccounts.windowFiveHour', "5-hour limit");
		case 'seven_day': return localize('wispAccounts.windowSevenDay', "7-day limit");
		case 'primary': return localize('wispAccounts.windowPrimary', "Primary limit");
		case 'secondary': return localize('wispAccounts.windowSecondary', "Secondary limit");
		default: return window;
	}
}
