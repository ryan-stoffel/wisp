/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { Event } from '../../../../base/common/event.js';
import { localize } from '../../../../nls.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import type { DetectedCli } from '../../../../platform/wisp/common/wispProtocol.js';
import { buildSignInLaunch } from '../../../../platform/wisp/common/wispSignIn.js';
import { ITerminalService } from '../../../../workbench/contrib/terminal/browser/terminal.js';
import { cliLabel, IWispAccountsService } from './wispAccounts.js';

/**
 * Opens an integrated terminal running a detected CLI's vendor login command (decision record
 * 0004, #115): the plain command on `local`, or `ssh -t -- <destination> <cli> <login args>` on a
 * remote host, built by {@link buildSignInLaunch} with no shell involved on either side. wisp
 * never reads what the terminal prints, so it never sees the credentials this produces; once the
 * terminal's process exits, it only asks wispd to probe the CLI again, the same request the
 * Accounts view's own Refresh button makes.
 */
export class WispSignInFlow {

	constructor(
		@ITerminalService private readonly terminalService: ITerminalService,
		@IWispAccountsService private readonly accountsService: IWispAccountsService,
		@INotificationService private readonly notificationService: INotificationService,
	) { }

	/** `host` is `wisp.host` as read when the user clicked Sign in: `local`, or an ssh destination. */
	async run(cli: DetectedCli, host: string): Promise<void> {
		const result = buildSignInLaunch(cli.cli, host);
		if (result.kind === 'error') {
			this.notificationService.error(localize('wispSignIn.invalidHost', "Can't sign in to {0}: {1}", cliLabel(cli.cli), result.message));
			return;
		}

		const { launch } = result;
		try {
			const instance = await this.terminalService.createAndFocusTerminal({
				config: {
					name: localize('wispSignIn.terminalName', "Sign in: {0}", cliLabel(cli.cli)),
					executable: launch.executable,
					args: [...launch.args],
					env: launch.env,
				},
			});
			Event.once(instance.onExit)(() => {
				this.accountsService.refreshClis();
			});
		} catch (error) {
			this.notificationService.error(localize('wispSignIn.terminalFailed', "Couldn't open a terminal to sign in to {0}: {1}", cliLabel(cli.cli), toErrorMessage(error)));
		}
	}
}
