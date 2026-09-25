/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../../platform/configuration/common/configurationRegistry.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import './coordinatorChat.contribution.js';

// Registered in code so the first launch gets them too.
Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).registerDefaultConfigurations([{
	overrides: {
		// Upstream's own switch for Chat and Copilot.
		'chat.disableAIFeatures': true,
		// The coordinator chat is open at startup in every window, empty ones included, instead of the welcome page.
		'workbench.secondarySideBar.defaultVisibility': 'visible',
		'workbench.startupEditor': 'none',
	},
}]);
