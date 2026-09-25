/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispHostMenu.css';
import { Codicon } from '../../../../base/common/codicons.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { ConfigurationTarget, IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService, IQuickPickItem, IQuickPickSeparator } from '../../../../platform/quickinput/common/quickInput.js';
import { isLocalHost, validateSshDestination, WISP_HOST_SETTING } from '../../../../platform/wisp/common/wispdConfiguration.js';
import { IOutputService } from '../../../../workbench/services/output/common/output.js';
import { IWispHostStatus, WispHostMark } from './wispHostStatus.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_SHOW_HOST_MENU_COMMAND = 'wisp.host.showMenu';
export const WISP_SWITCH_HOST_COMMAND = 'wisp.host.switch';
export const WISP_SHOW_LOG_COMMAND = 'wisp.host.showLog';
export const WISP_RETRY_COMMAND = 'wisp.host.retry';

/** The shared process's wispd logger registers this output channel (platform/wisp/node/wispdService.ts). */
const WISPD_LOG_CHANNEL = 'wispd';

const category = localize2('wisp', "Wisp");

type HostMenuItem = IQuickPickItem & { readonly run?: () => unknown };

const MARK_ICONS: Record<WispHostMark, ThemeIcon> = {
	connected: Codicon.circleFilled,
	connecting: ThemeIcon.modify(Codicon.loading, 'spin'),
	idle: Codicon.circleLargeOutline,
	error: Codicon.error,
};

function markClasses(mark: WispHostMark): string[] {
	return [...ThemeIcon.asClassNameArray(MARK_ICONS[mark]), `wisp-host-mark-${mark}`];
}

function capitalize(text: string): string {
	return text.charAt(0).toUpperCase() + text.slice(1);
}

/** The current host's row: the state as its description, and what wispd or the problem says as its detail. */
function currentHostDetail(status: IWispHostStatus, remote: boolean): string | undefined {
	if (status.kind === 'connected' && status.wispd) {
		return remote
			? localize('wispHostMenu.connectedRemote', "wispd {0} over ssh", status.wispd)
			: localize('wispHostMenu.connectedLocal', "wispd {0} on this Mac", status.wispd);
	}
	return status.kind === 'error' ? status.heading : undefined;
}

export function hostMenuItems(status: IWispHostStatus, configuredHost: string, run: { retry(): unknown; useThisMac(): unknown; addHost(): unknown; showLog(): unknown }): Array<HostMenuItem | IQuickPickSeparator> {
	const remote = !isLocalHost(configuredHost);
	const items: Array<HostMenuItem | IQuickPickSeparator> = [
		{ type: 'separator', label: localize('wispHostMenu.hosts', "Hosts") },
		{
			id: 'current',
			label: remote ? status.host : capitalize(status.host),
			description: capitalize(status.state),
			detail: currentHostDetail(status, remote),
			iconClasses: markClasses(status.mark),
			ariaLabel: status.ariaLabel,
		},
	];
	if (remote) {
		items.push({
			id: 'local',
			label: localize('wispHostMenu.thisMac', "This Mac"),
			detail: localize('wispHostMenu.thisMacDetail', "Use the wispd that comes with Wisp"),
			iconClasses: ThemeIcon.asClassNameArray(Codicon.vm),
			run: run.useThisMac,
		});
	}
	items.push(
		{ id: 'add', label: localize('wispHostMenu.addHost', "Add Host..."), detail: localize('wispHostMenu.addHostDetail', "Connect to an ssh destination from your ssh config"), iconClasses: ThemeIcon.asClassNameArray(Codicon.server), run: run.addHost },
		{ type: 'separator' },
		{ id: 'reconnect', label: localize('wispHostMenu.reconnect', "Reconnect"), iconClasses: ThemeIcon.asClassNameArray(Codicon.refresh), run: run.retry },
		{ id: 'log', label: localize('wispHostMenu.showLog', "Show wispd Log"), iconClasses: ThemeIcon.asClassNameArray(Codicon.output), run: run.showLog },
	);
	return items;
}

async function writeHost(accessor: ServicesAccessor, host: string | undefined): Promise<void> {
	const configurationService = accessor.get(IConfigurationService);
	const notificationService = accessor.get(INotificationService);
	try {
		// Application-scoped, so it lands in the default profile's settings, which the shared process reads.
		await configurationService.updateValue(WISP_HOST_SETTING, host, ConfigurationTarget.USER);
	} catch (error) {
		notificationService.error(localize('wispHostMenu.writeFailed', "Couldn't change Wisp: Host: {0}", error instanceof Error ? error.message : String(error)));
	}
}

/** Asks for an ssh destination, checked the way the ssh transport checks it, and switches to it. */
async function addHost(accessor: ServicesAccessor): Promise<void> {
	const quickInputService = accessor.get(IQuickInputService);
	const statusService = accessor.get(IWispHostStatusService);
	const current = statusService.configuredHost.get();
	const destination = await quickInputService.input({
		title: localize('wispHostMenu.addHostTitle', "Add Host"),
		prompt: localize('wispHostMenu.addHostPrompt', "An ssh destination from your ssh config, such as a Host entry or user@host. wisp runs wispd there over ssh and stores no credentials."),
		placeHolder: 'mac-mini',
		value: isLocalHost(current) ? '' : current,
		ignoreFocusLost: true,
		validateInput: async value => {
			const problem = validateSshDestination(value.trim());
			return problem ? localize('wispHostMenu.invalidDestination', "This isn't a usable ssh destination: {0}.", problem) : undefined;
		},
	});
	if (destination === undefined) {
		return;
	}
	await writeHost(accessor, destination.trim());
}

async function showLog(accessor: ServicesAccessor): Promise<void> {
	const outputService = accessor.get(IOutputService);
	const notificationService = accessor.get(INotificationService);
	if (!outputService.getChannelDescriptor(WISPD_LOG_CHANNEL)) {
		notificationService.info(localize('wispHostMenu.noLog', "The wispd log isn't available yet. It appears once wisp first tries to connect."));
		return;
	}
	await outputService.showChannel(WISPD_LOG_CHANNEL);
}

/** Opens the host menu: the current host and its state, switching hosts, Reconnect, and the log. */
async function showHostMenu(accessor: ServicesAccessor): Promise<void> {
	const quickInputService = accessor.get(IQuickInputService);
	const statusService = accessor.get(IWispHostStatusService);
	const commandService = accessor.get(ICommandService);

	const store = new DisposableStore();
	const picker = store.add(quickInputService.createQuickPick<HostMenuItem>({ useSeparators: true }));
	picker.placeholder = localize('wispHostMenu.placeholder', "Choose a host, or an action for the current one");
	picker.matchOnDescription = true;
	const run = {
		retry: () => commandService.executeCommand(WISP_RETRY_COMMAND),
		useThisMac: () => commandService.executeCommand(WISP_SWITCH_HOST_COMMAND, 'local'),
		addHost: () => commandService.executeCommand(WISP_SWITCH_HOST_COMMAND),
		showLog: () => commandService.executeCommand(WISP_SHOW_LOG_COMMAND),
	};
	// Follows the state while open, so Reconnect's progress shows in place.
	store.add(autorun(reader => {
		const status = statusService.status.read(reader);
		const configuredHost = statusService.configuredHost.read(reader);
		const items = hostMenuItems(status, configuredHost, run);
		picker.items = items;
		if (!picker.activeItems.length) {
			const current = items.find((item): item is HostMenuItem => item.type !== 'separator' && item.id === 'current');
			picker.activeItems = current ? [current] : [];
		}
	}));

	await new Promise<void>(resolve => {
		store.add(picker.onDidAccept(() => {
			const [item] = picker.selectedItems;
			picker.hide();
			item?.run?.();
		}));
		store.add(picker.onDidHide(() => resolve()));
		picker.show();
	});
	store.dispose();
}

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: WISP_SHOW_HOST_MENU_COMMAND,
			title: localize2('wispHostMenu.show', "Show Host Menu"),
			category,
			f1: true,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return showHostMenu(accessor);
	}
});

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: WISP_SWITCH_HOST_COMMAND,
			title: localize2('wispHostMenu.switch', "Switch Host..."),
			category,
			f1: true,
		});
	}

	/** With `'local'`, switches to this Mac; otherwise asks for an ssh destination. */
	run(accessor: ServicesAccessor, host?: unknown): Promise<void> {
		return host === 'local' ? writeHost(accessor, undefined) : addHost(accessor);
	}
});

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: WISP_SHOW_LOG_COMMAND,
			title: localize2('wispHostMenu.showLogCommand', "Show wispd Log"),
			category,
			f1: true,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return showLog(accessor);
	}
});

registerAction2(class extends Action2 {
	constructor() {
		super({
			id: WISP_RETRY_COMMAND,
			title: localize2('wispHostMenu.retry', "Reconnect to Host"),
			category,
			f1: false,
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return accessor.get(IWispHostStatusService).retry();
	}
});

