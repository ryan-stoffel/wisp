/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Disposable } from '../../../../base/common/lifecycle.js';
import { autorun, derived, IObservable } from '../../../../base/common/observable.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { ICustomViewService } from '../../../services/customView/browser/customViewService.js';
import { IWispHostStatusService } from './wispHostStatusService.js';
import { WISP_NO_HOST_VIEW_ID, WispNoHostView } from './wispNoHostView.js';

/**
 * Shows the no-host view over the session surface while no host is connected, and hides it once
 * one is, uncovering the session surface.
 */
export class WispNoHostContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispNoHost';

	constructor(
		@ICustomViewService customViewService: ICustomViewService,
		@IWispHostStatusService hostStatusService: IWispHostStatusService,
	) {
		super();

		const hostConnected: IObservable<boolean> = derived(reader => hostStatusService.status.read(reader).kind === 'connected');

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
