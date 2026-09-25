/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Codicon } from '../../../../base/common/codicons.js';
import { KeyCode, KeyMod } from '../../../../base/common/keyCodes.js';
import { localize, localize2 } from '../../../../nls.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import { registerIcon } from '../../../../platform/theme/common/iconRegistry.js';
import { ViewPaneContainer } from '../../../browser/parts/views/viewPaneContainer.js';
import { IViewContainersRegistry, IViewsRegistry, ViewContainer, ViewContainerLocation, Extensions as ViewExtensions } from '../../../common/views.js';
import { COORDINATOR_CHAT_VIEW_ID, COORDINATOR_VIEW_CONTAINER_ID, CoordinatorChatViewPane } from './coordinatorChatView.js';

const coordinatorViewIcon = registerIcon('wisp-coordinator-view-icon', Codicon.commentDiscussion, localize('coordinatorViewIcon', "View icon of the coordinator chat."));

// The secondary side bar's only default container, because wisp's registration filter keeps upstream
// Chat's out. The layout reads it at startup, so it must register on import, not from an extension.
const coordinatorViewContainer: ViewContainer = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry).registerViewContainer({
	id: COORDINATOR_VIEW_CONTAINER_ID,
	title: localize2('coordinator', "Coordinator"),
	icon: coordinatorViewIcon,
	ctorDescriptor: new SyncDescriptor(ViewPaneContainer, [COORDINATOR_VIEW_CONTAINER_ID, { mergeViewWithContainerWhenSingleView: true }]),
	storageId: COORDINATOR_VIEW_CONTAINER_ID,
	order: 0,
}, ViewContainerLocation.AuxiliaryBar, { isDefault: true });

Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).registerViews([{
	id: COORDINATOR_CHAT_VIEW_ID,
	name: localize2('coordinatorChat', "Chat"),
	containerIcon: coordinatorViewContainer.icon,
	containerTitle: coordinatorViewContainer.title.value,
	singleViewPaneContainerTitle: coordinatorViewContainer.title.value,
	canToggleVisibility: false,
	canMoveView: true,
	// Upstream Chat's chord, which is free while chat.disableAIFeatures is on.
	focusCommand: {
		id: `${COORDINATOR_CHAT_VIEW_ID}.focus`,
		keybindings: {
			primary: KeyMod.CtrlCmd | KeyMod.Alt | KeyCode.KeyI,
			mac: { primary: KeyMod.CtrlCmd | KeyMod.WinCtrl | KeyCode.KeyI },
		},
	},
	ctorDescriptor: new SyncDescriptor(CoordinatorChatViewPane),
}], coordinatorViewContainer);
