/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispProject.css';
import { $, append, clearNode } from '../../../../base/browser/dom.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { fromNow } from '../../../../base/common/date.js';
import { autorun, observableValue } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { IContextKeyService } from '../../../../platform/contextkey/common/contextkey.js';
import { IContextMenuService } from '../../../../platform/contextview/browser/contextView.js';
import { IHoverService } from '../../../../platform/hover/browser/hover.js';
import { IInstantiationService } from '../../../../platform/instantiation/common/instantiation.js';
import { IKeybindingService } from '../../../../platform/keybinding/common/keybinding.js';
import { IOpenerService } from '../../../../platform/opener/common/opener.js';
import { IThemeService } from '../../../../platform/theme/common/themeService.js';
import { isLocalHost } from '../../../../platform/wisp/common/wispdConfiguration.js';
import type { Project } from '../../../../platform/wisp/common/wispProtocol.js';
import { IViewPaneOptions, ViewPane } from '../../../../workbench/browser/parts/views/viewPane.js';
import { IViewDescriptorService } from '../../../../workbench/common/views.js';
import { IPathService } from '../../../../workbench/services/path/common/pathService.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { projectGlyph, projectIdOf, tildify, WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_PROJECT_CONTAINER_ID = 'wisp.project';
export const WISP_PROJECT_VIEW_ID = 'wisp.project.view';

/** One line of the project's checklist: done facts get a check, pending ones a hollow circle. */
export interface IWispProjectFact {
	readonly id: 'repo' | 'plan' | 'context' | 'account';
	readonly label: string;
	readonly detail: string;
	readonly done: boolean;
}

/**
 * The facts the Project tab lists (docs/design/agents-window.md, note 10). The repository comes
 * from wispd; the rest say plainly what wisp doesn't have yet: a plan (M4), shared context (M3),
 * and the coordinator's account (M2).
 */
export function projectFacts(project: Project, host: string, home: string | undefined): IWispProjectFact[] {
	const path = tildify(project.repoPath, home);
	return [
		{
			id: 'repo',
			label: localize('wispProject.repo', "Repository"),
			detail: project.branch
				? localize('wispProject.repoDetail', "{0} on {1}, branch {2}", path, host, project.branch)
				: localize('wispProject.repoDetailNoBranch', "{0} on {1}", path, host),
			done: true,
		},
		{ id: 'plan', label: localize('wispProject.plan', "Plan"), detail: localize('wispProject.planNone', "None yet"), done: false },
		{ id: 'context', label: localize('wispProject.context', "Shared context"), detail: localize('wispProject.contextNone', "0 files"), done: false },
		{ id: 'account', label: localize('wispProject.account', "Coordinator account"), detail: localize('wispProject.accountNone', "Not set"), done: false },
	];
}

/**
 * The right panel's Project tab: the open project's name, where its coordinator runs, and a
 * checklist of facts. It follows the active session, and says so when that isn't a project.
 */
export class WispProjectView extends ViewPane {

	private readonly home = observableValue<string | undefined>(this, undefined);

	constructor(
		options: IViewPaneOptions,
		@IKeybindingService keybindingService: IKeybindingService,
		@IContextMenuService contextMenuService: IContextMenuService,
		@IConfigurationService configurationService: IConfigurationService,
		@IContextKeyService contextKeyService: IContextKeyService,
		@IViewDescriptorService viewDescriptorService: IViewDescriptorService,
		@IInstantiationService instantiationService: IInstantiationService,
		@IOpenerService openerService: IOpenerService,
		@IThemeService themeService: IThemeService,
		@IHoverService hoverService: IHoverService,
		@ISessionsService private readonly sessionsService: ISessionsService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IPathService pathService: IPathService,
	) {
		super(options, keybindingService, contextMenuService, configurationService, contextKeyService, viewDescriptorService, instantiationService, openerService, themeService, hoverService);
		pathService.userHome().then(home => this.home.set(home.fsPath, undefined), () => { /* keep full paths */ });
	}

	protected override renderBody(container: HTMLElement): void {
		super.renderBody(container);
		const root = append(container, $('.wisp-project'));

		this._register(autorun(reader => {
			const active = this.sessionsService.activeSession.read(reader);
			const id = active?.sessionType === WISP_PROJECT_SESSION_TYPE ? projectIdOf(active.resource) : undefined;
			const project = id ? this.projectsService.projects.read(reader).find(candidate => candidate.id === id) : undefined;
			const status = this.hostStatusService.status.read(reader);
			const local = isLocalHost(this.hostStatusService.configuredHost.read(reader));
			clearNode(root);
			if (!project) {
				append(root, $('p.wisp-project-empty', undefined, localize('wispProject.none', "Open a project to see its repository, plan, and shared context.")));
				return;
			}
			this.renderProject(root, project, status.host, local ? this.home.read(reader) : undefined);
		}));
	}

	private renderProject(root: HTMLElement, project: Project, host: string, home: string | undefined): void {
		const header = append(root, $('.wisp-project-header'));
		const glyph = projectGlyph(project);
		append(header, $('span.wisp-project-glyph', { 'aria-hidden': 'true', 'data-color': glyph.color }, glyph.letter));
		append(header, $('h2.wisp-project-name', undefined, project.name));
		append(root, $('p.wisp-project-subtitle', undefined, localize('wispProject.subtitle', "Coordinator on {0}, created {1}", host, fromNow(new Date(project.createdAt), true))));

		const list = append(root, $('ul.wisp-project-facts', { 'aria-label': localize('wispProject.factsLabel', "Project facts") }));
		for (const fact of projectFacts(project, host, home)) {
			const item = append(list, $('li.wisp-project-fact', { 'data-fact': fact.id, 'data-done': String(fact.done) }));
			const icon = fact.done ? Codicon.check : Codicon.circleLargeOutline;
			append(item, $(`span.wisp-project-fact-icon${ThemeIcon.asCSSSelector(icon)}`, { 'aria-hidden': 'true' }));
			const text = append(item, $('.wisp-project-fact-text'));
			append(text, $('span.wisp-project-fact-label', undefined, fact.label));
			append(text, $('span.wisp-project-fact-detail', undefined, fact.detail));
		}
	}
}
