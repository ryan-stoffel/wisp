/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { localize } from '../../../nls.js';
import { ConfigurationScope, Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../configuration/common/configurationRegistry.js';
import { Registry } from '../../registry/common/platform.js';

/** `local`, or an ssh destination from the user's ssh config (decision record 0007). */
export const WISP_HOST_SETTING = 'wisp.host';
export const WISP_HOST_LOCAL = 'local';

/** The absolute path to `wispd` on the host named by {@link WISP_HOST_SETTING}. Empty means "detect it". */
export const WISP_REMOTE_WISPD_PATH_SETTING = 'wisp.remoteWispdPath';

/**
 * Where the editor's wispd is, and how to reach it there. Both are application-scoped: a remote
 * host is a property of this Mac, not of whatever repo happens to be open, so a workspace's
 * `.vscode/settings.json` can't set them (decision record 0007). Imported from `node/wispdService.ts`
 * so the shared process's own configuration registry has these too, and from the workbench's
 * `wisp.contribution.ts` so the editor window's settings UI and scope enforcement see them.
 */
Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).registerConfiguration({
	id: 'wisp',
	title: 'Wisp',
	type: 'object',
	properties: {
		[WISP_HOST_SETTING]: {
			type: 'string',
			default: WISP_HOST_LOCAL,
			scope: ConfigurationScope.APPLICATION,
			markdownDescription: localize(
				'wisp.host',
				"Where the editor's wispd runs: `{0}`, or an ssh destination from your ssh config, such as a `Host` entry. wisp stores no credentials; your ssh config, keys, and agent do the authentication. Changing this takes effect after reloading the window.",
				WISP_HOST_LOCAL,
			),
		},
		[WISP_REMOTE_WISPD_PATH_SETTING]: {
			type: 'string',
			default: '',
			scope: ConfigurationScope.APPLICATION,
			markdownDescription: localize(
				'wisp.remoteWispdPath',
				"The absolute path to `wispd` on the host named by `#wisp.host#`. Left empty, the editor tries `wispd` on the host's `PATH`, then `/opt/homebrew/bin/wispd`, then `/usr/local/bin/wispd`, without a login shell: a non-interactive SSH command's `PATH` usually leaves Homebrew out (see daemon/README.md). Setting this skips that search, at the cost of having to update it if wispd ever moves.",
			),
		},
	},
});
