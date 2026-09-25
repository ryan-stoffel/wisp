/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { localize } from '../../../nls.js';
import { ConfigurationScope, Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../configuration/common/configurationRegistry.js';
import { Registry } from '../../registry/common/platform.js';

/** `local`, or an ssh destination from the user's ssh config (decision record 0007). */
export const WISP_HOST_SETTING = 'wisp.host';
export const WISP_HOST_LOCAL = 'local';

/** Every C0 and C1 control character, plus Unicode format characters such as zero-width space and RTL override. */
const CONTROL_OR_FORMAT_CHARACTER = /[\p{Cc}\p{Cf}]/u;
/** Shell metacharacters. ssh runs argv with no shell of its own, but the destination still ends up in one on the host (0007) or, for a user@-host form, can be misread by ssh itself. */
const SHELL_METACHARACTER = /[`$;&|<>(){}'"\\]/;
/** A `-` right where ssh would start reading a hostname: at the start, right after `@`, or right after a `scheme://`. */
const LEADING_DASH = /(^|@|:\/\/)-/;

/**
 * Rejects an ssh destination that ssh could misread as an option, that could carry stray or
 * invisible bytes into the argument vector, or that could inject a second shell command once the
 * host's login shell runs it (0007's `-- <destination>` stops ssh from reading it as an option,
 * but not the host's shell from interpreting what's inside it). Returns why it's invalid, or
 * `undefined` if it's fine to use.
 */
export function validateSshDestination(destination: string): string | undefined {
	if (destination.length === 0) {
		return 'it is empty';
	}
	if (LEADING_DASH.test(destination)) {
		return 'it starts with "-" (or a user or scheme part does), which ssh would read as an option';
	}
	if (/\s/.test(destination)) {
		return 'it contains whitespace';
	}
	if (CONTROL_OR_FORMAT_CHARACTER.test(destination)) {
		return 'it contains a control or invisible formatting character';
	}
	if (SHELL_METACHARACTER.test(destination)) {
		return 'it contains a shell metacharacter';
	}
	return undefined;
}

/** What the UI calls a `wisp.host` value: "this Mac" for `local`, otherwise the ssh destination. */
export function hostLabel(host: unknown, localLabel: string): string {
	return isLocalHost(host) ? localLabel : (host as string).trim();
}

/** Whether a `wisp.host` value means this Mac. Anything that is not a string counts as unset. */
export function isLocalHost(host: unknown): boolean {
	const trimmed = typeof host === 'string' ? host.trim() : '';
	return trimmed.length === 0 || trimmed === WISP_HOST_LOCAL;
}

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
				"Where the editor's wispd runs: `{0}`, or an ssh destination from your ssh config, such as a `Host` entry. wisp stores no credentials; your ssh config, keys, and agent do the authentication. Changing this reconnects right away.",
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
