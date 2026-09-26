/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Disposable } from '../../../../../base/common/lifecycle.js';
import { InstantiationType, registerSingleton } from '../../../../../platform/instantiation/common/extensions.js';
import { IInstantiationService } from '../../../../../platform/instantiation/common/instantiation.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../../workbench/common/contributions.js';
import { ISessionsProvidersService } from '../../../../services/sessions/browser/sessionsProvidersService.js';
import { WispAgentChatSessions } from './wispAgentChatSessions.js';
import { IWispAgentsService, WispAgentsService } from './wispAgentsService.js';
import { WispCoordinatorChat } from './wispCoordinatorChat.js';
import { IWispProjectsService, WispProjectsService } from './wispProjectsService.js';
import { WispSessionsProvider } from './wispSessionsProvider.js';

registerSingleton(IWispProjectsService, WispProjectsService, InstantiationType.Delayed);
registerSingleton(IWispAgentsService, WispAgentsService, InstantiationType.Delayed);

export class WispSessionsProviderContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispSessionsProvider';

	constructor(
		@IInstantiationService instantiationService: IInstantiationService,
		@ISessionsProvidersService sessionsProvidersService: ISessionsProvidersService,
	) {
		super();

		// The chat types first, so a restored project or subagent finds its content provider.
		this._register(instantiationService.createInstance(WispCoordinatorChat));
		this._register(instantiationService.createInstance(WispAgentChatSessions));
		const provider = this._register(instantiationService.createInstance(WispSessionsProvider));
		this._register(sessionsProvidersService.registerProvider(provider));
	}
}

registerWorkbenchContribution2(WispSessionsProviderContribution.ID, WispSessionsProviderContribution, WorkbenchPhase.AfterRestored);
