/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispProject.css';
import { $, addDisposableListener, append, clearNode, EventType } from '../../../../base/browser/dom.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { fromNow } from '../../../../base/common/date.js';
import { DisposableStore } from '../../../../base/common/lifecycle.js';
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
import type { ContextFile, Project } from '../../../../platform/wisp/common/wispProtocol.js';
import { IViewPaneOptions, ViewPane } from '../../../../workbench/browser/parts/views/viewPane.js';
import { IViewDescriptorService } from '../../../../workbench/common/views.js';
import { IEditorService } from '../../../../workbench/services/editor/common/editorService.js';
import { IPathService } from '../../../../workbench/services/path/common/pathService.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { projectGlyph, projectIdOf, tildify, WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';
import { toContextUri } from '../common/wispContextUri.js';
import { WispContextAddFileFlow } from './wispContextAddFile.js';
import { IWispContextService, WispContextState } from './wispContextService.js';
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
 * The facts the Project tab lists (docs/design/agents-window.md, note 10). The repository and the
 * shared context count come from wispd; the rest say plainly what wisp doesn't have yet: a plan
 * (M4) and the coordinator's account (M2). `contextFileCount` is omitted while the shared context
 * section below hasn't loaded yet, which reads the same as an empty one.
 */
export function projectFacts(project: Project, host: string, home: string | undefined, contextFileCount?: number): IWispProjectFact[] {
	const path = tildify(project.repoPath, home);
	const files = contextFileCount ?? 0;
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
		{
			id: 'context',
			label: localize('wispProject.context', "Shared context"),
			detail: files === 1 ? localize('wispProject.contextOneFile', "1 file") : localize('wispProject.contextFiles', "{0} files", files),
			done: files > 0,
		},
		{ id: 'account', label: localize('wispProject.account', "Coordinator account"), detail: localize('wispProject.accountNone', "Not set"), done: false },
	];
}

/** A fact's accessible name: "Repository, ~/src/app on this Mac, done". */
export function factAriaLabel(fact: IWispProjectFact): string {
	return fact.done
		? localize('wispProject.factDone', "{0}, {1}, done", fact.label, fact.detail)
		: localize('wispProject.factNotYet', "{0}, {1}, not yet", fact.label, fact.detail);
}

/** A shared context file's detail line: who last wrote it, and when, if wispd knows either. */
export function contextFileDetail(file: ContextFile): string {
	const when = fromNow(new Date(file.modifiedAt), true);
	return file.lastWriter
		? localize('wispProject.contextFileWriter', "{0} · {1}", file.lastWriter, when)
		: when;
}

/**
 * The right panel's Project tab: the open project's name, where its coordinator runs, and a
 * checklist of facts. It follows the active session, and says so when that isn't a project.
 */
export class WispProjectView extends ViewPane {

	private readonly home = observableValue<string | undefined>(this, undefined);
	private readonly contextStore = this._register(new DisposableStore());

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
		@IWispContextService private readonly contextService: IWispContextService,
		@IEditorService private readonly editorService: IEditorService,
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
			const context = project ? this.contextService.state(project.id).read(reader) : undefined;
			clearNode(root);
			if (!project) {
				append(root, $('p.wisp-project-empty', undefined, localize('wispProject.none', "Open a project to see its repository, plan, and shared context.")));
				return;
			}
			this.renderProject(root, project, status.host, local ? this.home.read(reader) : undefined, context ?? { kind: 'idle' });
		}));
	}

	private renderProject(root: HTMLElement, project: Project, host: string, home: string | undefined, context: WispContextState): void {
		const header = append(root, $('.wisp-project-header'));
		const glyph = projectGlyph(project);
		append(header, $('span.wisp-project-glyph', { 'aria-hidden': 'true', 'data-color': glyph.color }, glyph.letter));
		append(header, $('h2.wisp-project-name', undefined, project.name));
		append(root, $('p.wisp-project-subtitle', undefined, localize('wispProject.subtitle', "Coordinator on {0}, created {1}", host, fromNow(new Date(project.createdAt), true))));

		const fileCount = context.kind === 'ready' ? context.files.length : undefined;
		const list = append(root, $('ul.wisp-project-facts', { 'aria-label': localize('wispProject.factsLabel', "Project facts") }));
		for (const fact of projectFacts(project, host, home, fileCount)) {
			const item = append(list, $('li.wisp-project-fact', { 'data-fact': fact.id, 'data-done': String(fact.done) }));
			const icon = fact.done ? Codicon.check : Codicon.circleLargeOutline;
			append(item, $(`span.wisp-project-fact-icon${ThemeIcon.asCSSSelector(icon)}`, { 'aria-hidden': 'true' }));
			// The mark is hidden from screen readers, so the text they read says whether the fact is in place.
			const text = append(item, $('.wisp-project-fact-text', { 'aria-hidden': 'true' }));
			append(text, $('span.wisp-project-fact-label', undefined, fact.label));
			append(text, $('span.wisp-project-fact-detail', undefined, fact.detail));
			append(item, $('span.wisp-project-fact-spoken', undefined, factAriaLabel(fact)));
		}

		this.renderSharedContext(root, project, context);
	}

	/** The Shared context section (note 10): the project's files, and a `+` to add one. */
	private renderSharedContext(root: HTMLElement, project: Project, context: WispContextState): void {
		this.contextStore.clear();
		const section = append(root, $('.wisp-project-context'));
		const header = append(section, $('.wisp-project-context-header'));
		append(header, $('h3.wisp-project-context-title', undefined, localize('wispProject.contextTitle', "Shared context")));
		const addLabel = localize('wispProject.contextAdd', "Add a shared context file");
		const add = append(header, $<HTMLButtonElement>('button.wisp-project-context-add', { type: 'button', 'aria-label': addLabel }));
		append(add, $(`span${ThemeIcon.asCSSSelector(Codicon.add)}`, { 'aria-hidden': 'true' }));
		this.contextStore.add(addDisposableListener(add, EventType.CLICK, () => {
			this.instantiationService.createInstance(WispContextAddFileFlow).run(project.id);
		}));

		switch (context.kind) {
			case 'idle':
			case 'loading':
				append(section, $('p.wisp-project-context-empty', { 'aria-busy': 'true' }, localize('wispProject.contextLoading', "Loading shared context...")));
				return;
			case 'failed': {
				const failed = append(section, $('p.wisp-project-context-empty'));
				append(failed, $('span', undefined, context.message + ' '));
				const retry = append(failed, $<HTMLButtonElement>('button.wisp-project-context-retry', { type: 'button' }, localize('wispProject.contextRetry', "Retry")));
				this.contextStore.add(addDisposableListener(retry, EventType.CLICK, () => this.contextService.reload(project.id)));
				return;
			}
			case 'ready':
				if (context.files.length === 0) {
					append(section, $('p.wisp-project-context-empty', undefined, localize('wispProject.contextEmpty', "No shared context yet. Add a file every agent in the project can read.")));
					return;
				}
				this.renderContextFiles(section, project, context.files);
				return;
		}
	}

	private renderContextFiles(section: HTMLElement, project: Project, files: readonly ContextFile[]): void {
		const list = append(section, $('ul.wisp-project-context-files', { 'aria-label': localize('wispProject.contextFilesLabel', "Shared context files") }));
		for (const file of files) {
			const item = append(list, $('li.wisp-project-context-file'));
			const link = append(item, $<HTMLButtonElement>('button.wisp-project-context-link', { type: 'button', 'aria-label': localize('wispProject.contextFileLabel', "{0}, {1}", file.path, contextFileDetail(file)) }, file.path));
			this.contextStore.add(addDisposableListener(link, EventType.CLICK, () => {
				this.editorService.openEditor({ resource: toContextUri(project.id, file.path) });
			}));
			append(item, $('span.wisp-project-context-detail', { 'aria-hidden': 'true' }, contextFileDetail(file)));
		}
	}
}
