/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import '../browser/wisp.contribution.js';
import '../../../../platform/wisp/electron-browser/wispdService.js';

import { KeyCode, KeyMod } from '../../../../base/common/keyCodes.js';
import { Disposable } from '../../../../base/common/lifecycle.js';
import Severity from '../../../../base/common/severity.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { IContextKeyService, RawContextKey } from '../../../../platform/contextkey/common/contextkey.js';
import { ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { KeybindingWeight } from '../../../../platform/keybinding/common/keybindingsRegistry.js';
import { INativeHostService } from '../../../../platform/native/common/native.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IWispdService, WispdState, describeIncompatible } from '../../../../platform/wisp/common/wispd.js';
import { IWorkbenchContribution, WorkbenchPhase, registerWorkbenchContribution2 } from '../../../common/contributions.js';

/** The connection state's kind: `connecting`, `connected`, `disconnected`, or `incompatible`. */
export const WispdStateContext = new RawContextKey<string>('wisp.wispdState', 'disconnected', localize('wisp.wispdState', "The state of the connection to wispd"));

/**
 * The capabilities the connected wispd advertises. Gate a feature on its capability with
 * `'agents' in wisp.wispdCapabilities`, so it is hidden when wispd lacks it.
 */
export const WispdCapabilitiesContext = new RawContextKey<string[]>('wisp.wispdCapabilities', [], localize('wisp.wispdCapabilities', "The capabilities the connected wispd advertises"));

const category = localize2('wisp', "Wisp");

/** Starts the connection once the window has settled, and mirrors its state in context keys. */
class WispdConnectionContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'workbench.contrib.wisp.wispdConnection';

	constructor(
		@IWispdService wispdService: IWispdService,
		@IContextKeyService contextKeyService: IContextKeyService,
	) {
		super();
		const stateKey = WispdStateContext.bindTo(contextKeyService);
		const capabilitiesKey = WispdCapabilitiesContext.bindTo(contextKeyService);
		const update = (state: WispdState) => {
			stateKey.set(state.kind);
			capabilitiesKey.set(state.kind === 'connected' ? Object.keys(state.capabilities) : []);
		};
		this._register(wispdService.onDidChangeState(update));
		wispdService.getState().then(update);
	}
}

registerWorkbenchContribution2(WispdConnectionContribution.ID, WispdConnectionContribution, WorkbenchPhase.Eventually);

function describe(state: WispdState): string {
	switch (state.kind) {
		case 'connecting':
			return localize('wispd.connecting', "Connecting to wispd through {0} (attempt {1}).", state.command, state.attempt);
		case 'connected': {
			const capabilities = Object.keys(state.capabilities);
			return localize('wispd.connected', "Connected to wispd {0}, protocol {1}, through {2}. Capabilities: {3}.", state.wispd, state.protocol, state.command, capabilities.length ? capabilities.join(', ') : localize('wispd.none', "none"));
		}
		case 'disconnected': {
			const parts = [localize('wispd.disconnected', "Disconnected from wispd ({0}): {1}", state.reason, state.message)];
			if (state.stderr) {
				parts.push(state.stderr.split('\n').slice(-3).join(' '));
			}
			if (state.retryAt !== undefined) {
				parts.push(localize('wispd.retrying', "Retrying in {0} s.", Math.max(0, Math.ceil((state.retryAt - Date.now()) / 1000))));
			}
			return parts.join(' ');
		}
		case 'incompatible':
			return localize('wispd.incompatible', "wispd can't be used: {0}", describeIncompatible(state));
	}
}

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: 'wisp.wispd.showStatus',
			title: localize2('wisp.wispd.showStatus', "Show wispd Status"),
			category,
			f1: true,
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		const wispdService = accessor.get(IWispdService);
		const notificationService = accessor.get(INotificationService);

		const state = await wispdService.getState();
		let message = describe(state);
		if (state.kind === 'connected') {
			try {
				const health = await wispdService.request('host/health', {});
				message += ' ' + localize('wispd.health', "Up {0} s, store {1}, {2} running agents.", health.uptimeSeconds, health.store, health.runningAgents);
			} catch {
				// The state above already says enough.
			}
		}

		const severity = state.kind === 'incompatible' ? Severity.Error : state.kind === 'disconnected' ? Severity.Warning : Severity.Info;
		const choices = state.kind === 'connected' || state.kind === 'connecting'
			? []
			: [{ label: localize('wispd.retry', "Retry"), run: () => wispdService.retry() }];
		notificationService.prompt(severity, message, choices);
	}
});

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: 'wisp.wispd.reconnect',
			title: localize2('wisp.wispd.reconnect', "Reconnect to wispd"),
			category,
			f1: true,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return accessor.get(IWispdService).retry();
	}
});

// Upstream's Open Agents Window is disabled while AI features are off, which they are in this
// window. The Agents window is wisp's main UI, so the editor window keeps a way back to it.
registerAction2(class extends Action2 {
	constructor() {
		super({
			id: 'wisp.openAgentsWindow',
			title: localize2('wisp.openAgentsWindow', "Open Agents Window"),
			category,
			f1: true,
			keybinding: {
				primary: KeyMod.CtrlCmd | KeyMod.Shift | KeyCode.KeyA,
				weight: KeybindingWeight.WorkbenchContrib,
			},
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return accessor.get(INativeHostService).openAgentsWindow();
	}
});
