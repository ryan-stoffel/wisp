/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../../platform/configuration/common/configurationRegistry.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import '../../../../platform/wisp/common/wispdConfiguration.js';

// Upstream's own switches for Chat, Copilot, and TypeScript's automatic type acquisition.
// Registered in code so the first launch gets them too. chat.disableAIFeatures is also pinned
// in the editor window's configuration service (a strip: patch, #93) so a workspace or folder
// setting, trusted or not, and an extension's defaults cannot override this default; this
// override only decides what a user sees before they set their own value.
Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).registerDefaultConfigurations([{
	overrides: {
		'chat.disableAIFeatures': true,
		'typescript.disableAutomaticTypeAcquisition': true,
	},
}]);
