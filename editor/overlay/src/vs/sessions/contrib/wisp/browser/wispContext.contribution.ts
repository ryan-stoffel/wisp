/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './wispContextEditorBanner.js';
import { Disposable } from '../../../../base/common/lifecycle.js';
import { IFileService } from '../../../../platform/files/common/files.js';
import { InstantiationType, registerSingleton } from '../../../../platform/instantiation/common/extensions.js';
import { IInstantiationService } from '../../../../platform/instantiation/common/instantiation.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { WISP_CONTEXT_SCHEME } from '../common/wispContextUri.js';
import { WispContextFileSystemProvider } from './wispContextFileSystemProvider.js';
import { IWispContextService, WispContextService } from './wispContextService.js';

registerSingleton(IWispContextService, WispContextService, InstantiationType.Delayed);

/**
 * Registers the `wisp-context:` file system provider (docs/design/agents-window.md, note 10),
 * so a shared context file opens and saves through wispd wherever its URI is used: the Project
 * tab's section, and links to it in the coordinator's and agents' threads.
 */
export class WispContextFileSystemContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispContextFileSystem';

	constructor(
		@IInstantiationService instantiationService: IInstantiationService,
		@IFileService fileService: IFileService,
	) {
		super();
		const provider = this._register(instantiationService.createInstance(WispContextFileSystemProvider));
		this._register(fileService.registerProvider(WISP_CONTEXT_SCHEME, provider));
	}
}

registerWorkbenchContribution2(WispContextFileSystemContribution.ID, WispContextFileSystemContribution, WorkbenchPhase.AfterRestored);
