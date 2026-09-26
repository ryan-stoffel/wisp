/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

// wisp's browser code in the Agents window, which never loads the editor window's
// wisp.contribution.ts: the sessions provider, the sidebar view, and the host status. The
// window's entry point is ../electron-browser/wisp.sessions.contribution.ts, which imports this
// file and the wispd connection.

import { Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../../platform/configuration/common/configurationRegistry.js';
import { InstantiationType, registerSingleton } from '../../../../platform/instantiation/common/extensions.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import { IWispHostStatusService, WispHostStatusService } from './wispHostStatusService.js';
import { IWispAccountsService, WispAccountsService } from './wispAccounts.js';
import '../../providers/wisp/browser/wispSessionsProvider.contribution.js';
import './wispThreads.contribution.js';
import './wispNoHost.contribution.js';
import './wispHostMenu.js';
import './wispNewProject.js';
import './wispSearch.js';
import './wispProject.contribution.js';
import './wispContext.contribution.js';
import './wispComposerFooter.js';
import './wispAccountsEditor.js';
import './wispComposerAccount.js';
import './wispAgentsPanel.js';
import './wispStartSubagent.js';

registerSingleton(IWispHostStatusService, WispHostStatusService, InstantiationType.Delayed);
registerSingleton(IWispAccountsService, WispAccountsService, InstantiationType.Delayed);

// Settings that turn off what the exclusion list leaves behind. Registered in code so the
// first launch gets them too. In this window, a setting's own `agentsWindow.default` wins over
// these defaults, so a key added here must not have one; neither of these does.
Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).registerDefaultConfigurations([{
	overrides: {
		// The remote agent host commands (SSH, dev tunnels, WSL) and the workspace picker's Remote entry.
		'chat.remoteAgentHostsEnabled': false,
		// Copilot voice mode.
		'agents.voice.enabled': false,
		// The right panel keeps its own tab strip (Project, Changes, Files), as in the design,
		// instead of docking inside the editor with no tabs of its own.
		'sessions.layout.singlePaneDetailPanel': false,
	},
}]);
