/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Disposable } from '../../../../base/common/lifecycle.js';
import { autorun, constObservable, IObservable } from '../../../../base/common/observable.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { ICustomViewService } from '../../../services/customView/browser/customViewService.js';
import { WISP_NO_HOST_VIEW_ID, WispNoHostView } from './wispNoHostView.js';

/**
 * Shows the no-host view over the session surface while no host is connected. The Agents window
 * has no wispd connection yet, so for now that is always. #104 replaces `hostConnected` with the
 * real connection state.
 */
export class WispNoHostContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispNoHost';

	constructor(
		@ICustomViewService customViewService: ICustomViewService,
	) {
		super();

		const hostConnected: IObservable<boolean> = constObservable(false);

		this._register(customViewService.registerCustomView({
			id: WISP_NO_HOST_VIEW_ID,
			ctor: new SyncDescriptor(WispNoHostView),
		}, { restore: false }));

		this._register(autorun(reader => {
			if (hostConnected.read(reader)) {
				if (customViewService.activeCustomView.read(undefined)?.id === WISP_NO_HOST_VIEW_ID) {
					customViewService.hideCustomView();
				}
			} else {
				customViewService.showCustomView(WISP_NO_HOST_VIEW_ID);
			}
		}));
	}
}

// Before the layout restores, so the session surface never shows first.
registerWorkbenchContribution2(WispNoHostContribution.ID, WispNoHostContribution, WorkbenchPhase.BlockRestore);
