/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispThreads.css';
import { $, addDisposableListener, append, clearNode, EventType, getWindow } from '../../../../base/browser/dom.js';
import { status as ariaStatus } from '../../../../base/browser/ui/aria/aria.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { fromNow } from '../../../../base/common/date.js';
import { DisposableStore, MutableDisposable, toDisposable } from '../../../../base/common/lifecycle.js';
import { autorun, observableSignal, observableSignalFromEvent } from '../../../../base/common/observable.js';
import { basename } from '../../../../base/common/resources.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { ICommandService } from '../../../../platform/commands/common/commands.js';
import { IConfigurationService } from '../../../../platform/configuration/common/configuration.js';
import { IContextKeyService } from '../../../../platform/contextkey/common/contextkey.js';
import { IContextMenuService } from '../../../../platform/contextview/browser/contextView.js';
import { IHoverService } from '../../../../platform/hover/browser/hover.js';
import { IInstantiationService } from '../../../../platform/instantiation/common/instantiation.js';
import { IKeybindingService } from '../../../../platform/keybinding/common/keybinding.js';
import { IOpenerService } from '../../../../platform/opener/common/opener.js';
import { IThemeService } from '../../../../platform/theme/common/themeService.js';
import { IViewPaneOptions, ViewPane } from '../../../../workbench/browser/parts/views/viewPane.js';
import { IViewDescriptorService } from '../../../../workbench/common/views.js';
import { IPathService } from '../../../../workbench/services/path/common/pathService.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { ISession } from '../../../services/sessions/common/session.js';
import { ISessionsManagementService } from '../../../services/sessions/common/sessionsManagement.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { compactAge, projectGlyph, projectIdOf } from '../../providers/wisp/common/wispProjects.js';
import { WISP_SHOW_HOST_MENU_COMMAND } from './wispHostMenu.js';
import { IWispHostStatusService } from './wispHostStatusService.js';
import { WISP_NEW_PROJECT_COMMAND } from './wispNewProject.js';
import { projectSessions, WISP_SEARCH_COMMAND } from './wispSearch.js';

export const WISP_THREADS_CONTAINER_ID = 'wisp.threads';
export const WISP_THREADS_VIEW_ID = 'wisp.threads.view';

/** How often the rows' ages are refreshed. */
const AGE_REFRESH_MS = 60_000;

let instanceCount = 0;

/**
 * wisp's left sidebar in the Agents window (decision record 0011, docs/design/agents-window.md in
 * wisp): actions, then Projects, Repositories, and No Repo, then a footer with the user, the host,
 * and settings. Projects are read through `ISessionsManagementService` and opened through
 * `ISessionsService`. Normal threads, and with them New Chat, Repositories, and No Repo, come with
 * #110.
 */
export class WispThreadsView extends ViewPane {

	private root: HTMLElement | undefined;
	private firstAction: HTMLElement | undefined;

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
		@ICommandService private readonly commandService: ICommandService,
		@IPathService private readonly pathService: IPathService,
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@ISessionsManagementService private readonly sessionsManagementService: ISessionsManagementService,
		@ISessionsService private readonly sessionsService: ISessionsService,
	) {
		super(options, keybindingService, contextMenuService, configurationService, contextKeyService, viewDescriptorService, instantiationService, openerService, themeService, hoverService);
	}

	protected override renderBody(container: HTMLElement): void {
		super.renderBody(container);

		const idPrefix = `wisp-threads-${++instanceCount}`;
		const root = this.root = append(container, $('.wisp-threads'));

		const actions = append(root, $('.wisp-threads-actions', { role: 'group', 'aria-label': localize('wispThreads.actions', "Actions") }));
		this.firstAction = this.renderAction(actions, Codicon.edit, localize('wispThreads.newChat', "New Chat"), { disabledReason: localize('wispThreads.newChatDisabled', "Chats outside a project are not available yet.") });
		this.renderAction(actions, Codicon.search, localize('wispThreads.search', "Search"), { run: () => this.commandService.executeCommand(WISP_SEARCH_COMMAND), keybinding: WISP_SEARCH_COMMAND });
		// Automations stays hidden until wisp decides on triggers (M6).
		this.renderAction(actions, Codicon.settings, localize('wispThreads.customize', "Customize"), { disabledReason: localize('wispThreads.customizeDisabled', "Host and account settings are not available yet.") });

		const lists = append(root, $('.wisp-threads-lists'));
		this.renderProjects(lists, `${idPrefix}-projects`);
		// Repositories and No Repo render only once they have threads (#110).

		this.renderFooter(root);
	}

	private renderProjects(parent: HTMLElement, id: string): void {
		const { section, header } = this.renderSection(parent, id, localize('wispThreads.projects', "Projects"));
		const newProjectLabel = localize('wispThreads.newProject', "New Project");
		const newProject = append(header, $<HTMLButtonElement>('button.wisp-threads-icon-button.wisp-threads-new-project', { type: 'button', 'aria-label': newProjectLabel }));
		append(newProject, $(`span${ThemeIcon.asCSSSelector(Codicon.add)}`, { 'aria-hidden': 'true' }));
		const setNewProjectEnabled = this.enableable(newProject, newProjectLabel, () => this.commandService.executeCommand(WISP_NEW_PROJECT_COMMAND));

		const list = append(section, $('ul.wisp-threads-rows', { 'aria-labelledby': id }));
		const empty = append(section, $('.wisp-threads-empty'));
		const rowStore = this._register(new DisposableStore());
		const emptyStore = this._register(new DisposableStore());

		let tabStop: string | undefined;
		this._register(addDisposableListener(list, EventType.KEY_DOWN, event => {
			const rows = [...list.querySelectorAll<HTMLButtonElement>('button.wisp-threads-row')];
			const current = rows.indexOf(event.target as HTMLButtonElement);
			if (current === -1) {
				return;
			}
			let next: number;
			switch (event.key) {
				case 'ArrowDown': next = Math.min(current + 1, rows.length - 1); break;
				case 'ArrowUp': next = Math.max(current - 1, 0); break;
				case 'Home': next = 0; break;
				case 'End': next = rows.length - 1; break;
				default: return;
			}
			event.preventDefault();
			rows[current].tabIndex = -1;
			rows[next].tabIndex = 0;
			rows[next].focus();
		}));

		const sessionsChanged = observableSignalFromEvent(this, this.sessionsManagementService.onDidChangeSessions);
		const tick = observableSignal(this);
		const interval = getWindow(section).setInterval(() => tick.trigger(undefined), AGE_REFRESH_MS);
		this._register(toDisposable(() => getWindow(section).clearInterval(interval)));

		this._register(autorun(reader => {
			sessionsChanged.read(reader);
			tick.read(reader);
			const hostStatus = this.hostStatusService.status.read(reader);
			const state = this.projectsService.state.read(reader);
			const connected = hostStatus.kind === 'connected';
			setNewProjectEnabled(connected ? undefined : localize('wispThreads.newProjectDisabled', "Connect to a host to start a project."));

			const sessions = projectSessions(this.sessionsManagementService);
			for (const session of sessions) {
				session.title.read(reader);
				session.updatedAt.read(reader);
			}
			const active = this.sessionsService.activeSession.read(reader);
			const focused = list.ownerDocument.activeElement instanceof HTMLElement && list.contains(list.ownerDocument.activeElement)
				? list.ownerDocument.activeElement.dataset.session
				: undefined;

			rowStore.clear();
			clearNode(list);
			const now = Date.now();
			const rendered = sessions.map(session => this.renderRow(list, session, now, active?.resource.toString() === session.resource.toString(), rowStore));
			// One tab stop for the list: the row last focused, else the open project, else the first.
			const stop = rendered.find(row => row.dataset.session === (focused ?? tabStop))
				?? rendered.find(row => row.classList.contains('selected'))
				?? rendered[0];
			for (const row of rendered) {
				row.tabIndex = row === stop ? 0 : -1;
				rowStore.add(addDisposableListener(row, EventType.FOCUS, () => tabStop = row.dataset.session));
			}
			if (focused !== undefined) {
				stop?.focus();
			}
			list.hidden = sessions.length === 0;

			emptyStore.clear();
			clearNode(empty);
			empty.hidden = sessions.length > 0;
			if (sessions.length === 0) {
				this.renderEmpty(empty, connected, hostStatus.host, state, emptyStore);
			}
		}));
	}

	private renderRow(list: HTMLElement, session: ISession, now: number, selected: boolean, store: DisposableStore): HTMLButtonElement {
		const title = session.title.get();
		const updated = session.updatedAt.get();
		const item = append(list, $('li'));
		const row = append(item, $<HTMLButtonElement>('button.wisp-threads-row', { type: 'button' }));
		row.dataset.session = session.resource.toString();
		row.setAttribute('aria-label', localize('wispThreads.rowLabel', "{0}, project, updated {1}", title, fromNow(updated, true)));
		if (selected) {
			row.classList.add('selected');
			row.setAttribute('aria-current', 'true');
		}
		const glyph = projectGlyph({ id: projectIdOf(session.resource) ?? session.resource.toString(), name: title });
		append(row, $('span.wisp-threads-glyph', { 'aria-hidden': 'true', 'data-color': glyph.color }, glyph.letter));
		append(row, $('span.wisp-threads-row-title', { 'aria-hidden': 'true' }, title));
		append(row, $('span.wisp-threads-row-age', { 'aria-hidden': 'true' }, compactAge(updated, now)));
		store.add(addDisposableListener(row, EventType.CLICK, () => this.sessionsService.openSession(session.resource)));
		return row;
	}

	private renderEmpty(empty: HTMLElement, connected: boolean, host: string, state: ReturnType<IWispProjectsService['state']['get']>, store: DisposableStore): void {
		if (!connected) {
			append(empty, $('p', undefined, localize('wispThreads.noProjects', "No projects yet. A project runs on a host, so it shows here once wisp connects to one.")));
			return;
		}
		switch (state.kind) {
			case 'idle':
			case 'loading':
				empty.setAttribute('aria-busy', 'true');
				append(empty, $('p', undefined, localize('wispThreads.loading', "Loading projects...")));
				return;
			case 'failed': {
				empty.removeAttribute('aria-busy');
				append(empty, $('p', undefined, state.message));
				const retry = append(empty, $<HTMLButtonElement>('button.wisp-threads-link', { type: 'button' }, localize('wispThreads.retry', "Retry")));
				store.add(addDisposableListener(retry, EventType.CLICK, () => this.projectsService.reload()));
				return;
			}
			case 'ready': {
				empty.removeAttribute('aria-busy');
				append(empty, $('p', undefined, localize('wispThreads.noProjectsConnected', "No projects yet. A project is a repository on {0}, with a coordinator that plans the work.", host)));
				const create = append(empty, $<HTMLButtonElement>('button.wisp-threads-link', { type: 'button' }, localize('wispThreads.newProjectLink', "New Project")));
				store.add(addDisposableListener(create, EventType.CLICK, () => this.commandService.executeCommand(WISP_NEW_PROJECT_COMMAND)));
				return;
			}
		}
	}

	private renderAction(parent: HTMLElement, icon: ThemeIcon, label: string, options: { run?: () => void; disabledReason?: string; keybinding?: string }): HTMLButtonElement {
		const button = append(parent, $<HTMLButtonElement>('button.wisp-threads-action', { type: 'button' }));
		append(button, $(`span.wisp-threads-action-icon${ThemeIcon.asCSSSelector(icon)}`, { 'aria-hidden': 'true' }));
		append(button, $('span.wisp-threads-action-label', undefined, label));
		const keybinding = options.keybinding ? this.keybindingService.lookupKeybinding(options.keybinding)?.getLabel() : undefined;
		if (keybinding) {
			append(button, $('span.wisp-threads-action-keybinding', { 'aria-hidden': 'true' }, keybinding));
			button.setAttribute('aria-keyshortcuts', this.keybindingService.lookupKeybinding(options.keybinding!)?.getAriaLabel() ?? keybinding);
		}
		this.enableable(button, label, options.run)(options.disabledReason);
		return button;
	}

	/**
	 * Wires a button's click and returns a setter for why it is disabled (or `undefined` to enable
	 * it). A disabled button keeps focus and names its reason, through aria-disabled rather than
	 * the disabled attribute.
	 */
	private enableable(button: HTMLButtonElement, label: string, run: (() => unknown) | undefined): (disabledReason: string | undefined) => void {
		const hover = this._register(new MutableDisposable());
		let reason: string | undefined;
		if (run) {
			this._register(addDisposableListener(button, EventType.CLICK, () => {
				if (reason === undefined) {
					run();
				}
			}));
		}
		let first = true;
		return disabledReason => {
			if (!first && disabledReason === reason) {
				return;
			}
			first = false;
			reason = disabledReason;
			button.classList.toggle('disabled', reason !== undefined);
			if (reason === undefined) {
				button.removeAttribute('aria-disabled');
				button.removeAttribute('aria-description');
				hover.value = button.classList.contains('wisp-threads-icon-button') ? this.hoverService.setupDelayedHover(button, { content: label }) : undefined;
			} else {
				button.setAttribute('aria-disabled', 'true');
				button.setAttribute('aria-description', reason);
				hover.value = this.hoverService.setupDelayedHover(button, { content: reason });
			}
		};
	}

	private renderSection(parent: HTMLElement, id: string, title: string): { section: HTMLElement; header: HTMLElement } {
		const section = append(parent, $('section.wisp-threads-section', { 'aria-labelledby': id }));
		const header = append(section, $('.wisp-threads-section-header'));
		append(header, $('h2.wisp-threads-section-title', { id }, title));
		return { section, header };
	}

	private renderFooter(root: HTMLElement): void {
		const footer = append(root, $('.wisp-threads-footer'));

		const user = append(footer, $('.wisp-threads-user'));
		const avatar = append(user, $('span.wisp-threads-avatar', { 'aria-hidden': 'true' }));
		const name = append(user, $('span.wisp-threads-user-name'));
		this.pathService.userHome().then(home => {
			const accountName = basename(home);
			name.textContent = accountName;
			avatar.textContent = accountName.charAt(0).toUpperCase();
		});

		this.renderHostChip(footer);

		const settingsLabel = localize('wispThreads.settings', "Settings");
		const settings = append(footer, $<HTMLButtonElement>('button.wisp-threads-icon-button.wisp-threads-settings', { type: 'button', 'aria-label': settingsLabel }));
		append(settings, $(`span${ThemeIcon.asCSSSelector(Codicon.settingsGear)}`, { 'aria-hidden': 'true' }));
		this._register(this.hoverService.setupDelayedHover(settings, { content: settingsLabel }));
		this._register(addDisposableListener(settings, EventType.CLICK, () => this.commandService.executeCommand('workbench.action.openSettings')));
	}

	/**
	 * The host and its state, live from the wispd connection. The button's name carries both, so
	 * the visible parts are hidden from screen readers, and a change is announced once, politely.
	 */
	private renderHostChip(footer: HTMLElement): void {
		const chip = append(footer, $<HTMLButtonElement>('button.wisp-threads-host', { type: 'button' }));
		append(chip, $('span.wisp-threads-host-dot', { 'aria-hidden': 'true' }));
		const name = append(chip, $('span.wisp-threads-host-name', { 'aria-hidden': 'true' }));
		const state = append(chip, $('span.wisp-threads-host-state', { 'aria-hidden': 'true' }));
		const hover = this._register(new MutableDisposable());
		this._register(addDisposableListener(chip, EventType.CLICK, () => this.commandService.executeCommand(WISP_SHOW_HOST_MENU_COMMAND)));

		let announced: string | undefined;
		this._register(autorun(reader => {
			const status = this.hostStatusService.status.read(reader);
			chip.dataset.mark = status.mark;
			chip.dataset.kind = status.kind;
			chip.setAttribute('aria-label', status.ariaLabel);
			name.textContent = status.kind === 'notConnected' ? localize('wispThreads.notConnected', "Not connected") : status.host;
			state.textContent = status.showState && status.kind !== 'notConnected' ? status.state : '';
			hover.value = this.hoverService.setupDelayedHover(chip, { content: status.kind === 'error' ? `${status.ariaLabel}. ${status.heading}` : status.ariaLabel });
			// Connecting passes in a moment, so only where it lands is announced.
			if (status.kind === 'connecting') {
				return;
			}
			if (announced !== undefined && announced !== status.ariaLabel) {
				ariaStatus(status.ariaLabel);
			}
			announced = status.ariaLabel;
		}));
	}

	protected override layoutBody(height: number, width: number): void {
		super.layoutBody(height, width);
		if (this.root) {
			this.root.style.height = `${height}px`;
		}
	}

	override focus(): void {
		super.focus();
		this.firstAction?.focus();
	}
}
