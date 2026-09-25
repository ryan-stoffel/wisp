/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../base/common/codicons.js';
import { localize, localize2 } from '../../../../nls.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import { registerIcon } from '../../../../platform/theme/common/iconRegistry.js';
import { ViewPaneContainer } from '../../../../workbench/browser/parts/views/viewPaneContainer.js';
import { Extensions as ViewExtensions, IViewContainersRegistry, IViewsRegistry, ViewContainerLocation, WindowEnablement } from '../../../../workbench/common/views.js';
import { WISP_THREADS_CONTAINER_ID, WISP_THREADS_VIEW_ID, WispThreadsView } from './wispThreadsView.js';

// Replaces upstream's Sessions container, which the exclusion list leaves out (#103). It is the
// sidebar's default in the Agents window and is never registered in the editor window.

const threadsIcon = registerIcon('wisp-threads-view-icon', Codicon.commentDiscussion, localize('wispThreadsViewIcon', "Icon for wisp's projects and threads view."));
const threadsTitle = localize2('wispThreads.title', "Wisp");

const container = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry).registerViewContainer({
	id: WISP_THREADS_CONTAINER_ID,
	title: threadsTitle,
	icon: threadsIcon,
	ctorDescriptor: new SyncDescriptor(ViewPaneContainer, [WISP_THREADS_CONTAINER_ID, { mergeViewWithContainerWhenSingleView: true }]),
	storageId: WISP_THREADS_CONTAINER_ID,
	order: 0,
	windowEnablement: WindowEnablement.Sessions,
}, ViewContainerLocation.Sidebar, { isDefault: true, doNotRegisterOpenCommand: true });

Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).registerViews([{
	id: WISP_THREADS_VIEW_ID,
	name: threadsTitle,
	containerIcon: threadsIcon,
	containerTitle: threadsTitle.value,
	singleViewPaneContainerTitle: threadsTitle.value,
	ctorDescriptor: new SyncDescriptor(WispThreadsView),
	canToggleVisibility: false,
	canMoveView: false,
	windowEnablement: WindowEnablement.Sessions,
}], container);
