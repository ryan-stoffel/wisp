/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { disposableTimeout } from '../../../../base/common/async.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { Event } from '../../../../base/common/event.js';
import { Disposable, MutableDisposable } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { localize, localize2 } from '../../../../nls.js';
import { ContextKeyExpr } from '../../../../platform/contextkey/common/contextkey.js';
import { SyncDescriptor } from '../../../../platform/instantiation/common/descriptors.js';
import { Registry } from '../../../../platform/registry/common/platform.js';
import { registerIcon } from '../../../../platform/theme/common/iconRegistry.js';
import { ViewPaneContainer } from '../../../../workbench/browser/parts/views/viewPaneContainer.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { Extensions as ViewExtensions, IViewContainersRegistry, IViewDescriptorService, IViewsRegistry, ViewContainerLocation, WindowEnablement } from '../../../../workbench/common/views.js';
import { IViewsService } from '../../../../workbench/services/views/common/viewsService.js';
import { SessionTypeContext } from '../../../common/contextkeys.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';
import { WISP_PROJECT_CONTAINER_ID, WISP_PROJECT_VIEW_ID, WispProjectView } from './wispProjectView.js';

// The right panel's Project tab (decision record 0011): first, ahead of upstream's Changes (10)
// and Files (11), and shown only while a project is the active session.

const projectIcon = registerIcon('wisp-project-view-icon', Codicon.project, localize('wispProjectViewIcon', "Icon for wisp's Project tab."));
const projectTitle = localize2('wispProject.title', "Project");

export const WISP_PROJECT_CONTAINER = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry).registerViewContainer({
	id: WISP_PROJECT_CONTAINER_ID,
	title: projectTitle,
	icon: projectIcon,
	order: 0,
	ctorDescriptor: new SyncDescriptor(ViewPaneContainer, [WISP_PROJECT_CONTAINER_ID, { mergeViewWithContainerWhenSingleView: true }]),
	storageId: WISP_PROJECT_CONTAINER_ID,
	hideIfEmpty: true,
	windowEnablement: WindowEnablement.Sessions,
}, ViewContainerLocation.AuxiliaryBar, { isDefault: true, doNotRegisterOpenCommand: true });

Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).registerViews([{
	id: WISP_PROJECT_VIEW_ID,
	name: projectTitle,
	containerIcon: projectIcon,
	containerTitle: projectTitle.value,
	singleViewPaneContainerTitle: projectTitle.value,
	ctorDescriptor: new SyncDescriptor(WispProjectView),
	when: ContextKeyExpr.equals(SessionTypeContext.key, WISP_PROJECT_SESSION_TYPE),
	canToggleVisibility: false,
	canMoveView: false,
	windowEnablement: WindowEnablement.Sessions,
}], WISP_PROJECT_CONTAINER);

/**
 * Makes the Project tab the one a project opens on. Upstream's layout controller picks Files or
 * Changes for a session it hasn't seen, and hides the side panel for an existing one, so the first
 * time a project becomes active in this window its Project tab is opened, after that controller
 * has run. From then on the controller remembers what the user leaves open for that project.
 */
export class WispProjectTabContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispProjectTab';

	private readonly opened = new Set<string>();
	private readonly pending = this._register(new MutableDisposable());

	constructor(
		@ISessionsService sessionsService: ISessionsService,
		@IViewsService private readonly viewsService: IViewsService,
		@IViewDescriptorService private readonly viewDescriptorService: IViewDescriptorService,
	) {
		super();
		this._register(autorun(reader => {
			const active = sessionsService.activeSession.read(reader);
			if (active?.sessionType !== WISP_PROJECT_SESSION_TYPE) {
				return;
			}
			const key = active.resource.toString();
			if (this.opened.has(key)) {
				return;
			}
			this.opened.add(key);
			this.pending.value = disposableTimeout(() => this.open(), 0);
		}));
	}

	private open(): void {
		const model = this.viewDescriptorService.getViewContainerModel(WISP_PROJECT_CONTAINER);
		if (model.activeViewDescriptors.length > 0) {
			this.viewsService.openViewContainer(WISP_PROJECT_CONTAINER_ID, false);
			return;
		}
		// The view shows once the active session's type reaches the context.
		this.pending.value = Event.once(model.onDidChangeActiveViewDescriptors)(() => {
			this.viewsService.openViewContainer(WISP_PROJECT_CONTAINER_ID, false);
		});
	}
}

registerWorkbenchContribution2(WispProjectTabContribution.ID, WispProjectTabContribution, WorkbenchPhase.AfterRestored);
