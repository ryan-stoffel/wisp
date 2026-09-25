/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

// wisp's entry point in the Agents window, which never loads the editor window's
// wisp.contribution.ts. wisp's sessions provider and sidebar view are imported here.

import { Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../../platform/configuration/common/configurationRegistry.js';
import { Registry } from '../../../../platform/registry/common/platform.js';

// Settings that turn off what the exclusion list leaves behind. Registered in code so the
// first launch gets them too.
Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).registerDefaultConfigurations([{
	overrides: {
		// The remote agent host commands (SSH, dev tunnels, WSL) and the workspace picker's Remote entry.
		'chat.remoteAgentHostsEnabled': false,
		// Copilot voice mode.
		'agents.voice.enabled': false,
	},
}]);
