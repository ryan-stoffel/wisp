/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import './media/wispAgents.css';
import { $, addDisposableListener, append, EventType, getWindow, reset } from '../../../../base/browser/dom.js';
import { status } from '../../../../base/browser/ui/aria/aria.js';
import { AnchorAxisAlignment, AnchorPosition } from '../../../../base/browser/ui/contextview/contextview.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { Disposable, DisposableStore, IDisposable, toDisposable } from '../../../../base/common/lifecycle.js';
import { autorun, IObservable, IReader } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { URI } from '../../../../base/common/uri.js';
import { localize } from '../../../../nls.js';
import { IContextViewService } from '../../../../platform/contextview/browser/contextView.js';
import { isLocalHost } from '../../../../platform/wisp/common/wispdConfiguration.js';
import type { IChatPillEntry, IChatPillSection } from '../../../../workbench/browser/chatPills.js';
import { IChatPillPanel, registerChatPillPanel } from '../../../../workbench/browser/chatDropdownPill.js';
import { IWorkbenchContribution, registerWorkbenchContribution2, WorkbenchPhase } from '../../../../workbench/common/contributions.js';
import { agentAriaLabel, agentLocation, agentRunOf, agentState, agentTitle, IWispAgentState, WispAgentMark } from '../../providers/wisp/common/wispAgentRuns.js';
import { IWispAgentsService } from '../../providers/wisp/browser/wispAgentsService.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

/** The widget id of upstream's subagents pill (`sessionSubagentsPillOptions`). */
export const SUBAGENTS_PILL_WIDGET_ID = 'sessionSubagents';

/** Rows the panel shows before **More**. */
export const AGENTS_PANEL_ROWS = 5;

/** One row of the Agents panel. */
export interface IWispAgentRow {
	readonly entry: IChatPillEntry;
	readonly title: string;
	readonly state: IWispAgentState | undefined;
	readonly location: string;
	readonly isLocal: boolean;
	readonly ariaLabel: string;
}

/** The pill's summary: its label, count, and running mark. */
export interface IWispAgentsSummary {
	readonly count: number;
	readonly running: number;
	readonly ariaLabel: string;
}

export function agentsSummary(rows: readonly Pick<IWispAgentRow, 'state'>[]): IWispAgentsSummary {
	const running = rows.filter(row => row.state?.running).length;
	return {
		count: rows.length,
		running,
		ariaLabel: localize('wispAgents.pillAria', "Agents, {0}, {1} running", rows.length, running),
	};
}

/**
 * Wisp's presentation of upstream's subagents pill (decision record 0011, #105): the pill reads
 * **Agents**, its count, and a running mark, and opens a card over the bottom of the transcript
 * instead of a menu. The card lists the coordinator's subagents newest first: a status mark, the
 * task, where it runs, and the state as text; five rows, then **More**. A row opens its subagent
 * as a tab next to the coordinator.
 *
 * Keyboard: Up and Down move between rows, Home and End jump, Enter or Space opens a row, and
 * Escape or the close button closes the card and returns focus to the pill.
 */
export class WispAgentsPanel implements IChatPillPanel {

	constructor(
		private readonly agentsService: IWispAgentsService,
		private readonly hostStatusService: IWispHostStatusService,
		private readonly contextViewService: IContextViewService,
	) { }

	/** The row for a pill entry; a subagent that isn't wisp's keeps upstream's label and badge. */
	row(entry: IChatPillEntry, reader: IReader | undefined): IWispAgentRow {
		const isLocal = isLocalHost(this.hostStatusService.configuredHost.read(reader));
		const host = this.hostStatusService.status.read(reader).host;
		const ids = parseRun(entry.id);
		const run = ids ? this.agentsService.runs(ids.projectId).read(reader).find(candidate => candidate.id === ids.runId) : undefined;
		const state = run ? agentState(run, this.agentsService.step(run.id).read(reader)) : undefined;
		const location = run ? agentLocation(isLocal, host) : entry.badge ?? '';
		const stateLabel = state?.label ?? '';
		const title = run ? agentTitle(run.prompt) : entry.label;
		return {
			entry,
			title,
			state,
			location,
			isLocal: run ? isLocal : false,
			ariaLabel: stateLabel ? agentAriaLabel(title, stateLabel, location) : title,
		};
	}

	summary(entries: readonly IChatPillEntry[]): { readonly label: string; readonly icon?: ThemeIcon; readonly trailing?: readonly HTMLElement[]; readonly ariaLabel: string } {
		const summary = agentsSummary(entries.map(entry => this.row(entry, undefined)));
		const trailing: HTMLElement[] = [];
		if (summary.running > 0) {
			trailing.push($('span.wisp-agents-pill-running', { 'aria-hidden': 'true' }));
		}
		trailing.push($('span.wisp-agents-pill-count', { 'aria-hidden': 'true' }, String(summary.count)));
		return { label: localize('wispAgents.pill', "Agents"), trailing, ariaLabel: summary.ariaLabel };
	}

	show(trigger: HTMLElement, sections: IObservable<readonly IChatPillSection[]>, onDidHide: () => void): IDisposable {
		const store = new DisposableStore();
		let hidden = false;
		const hide = () => {
			if (!hidden) {
				hidden = true;
				this.contextViewService.hideContextView();
			}
		};
		this.contextViewService.showContextView({
			getAnchor: () => {
				const width = composerOf(trigger).getBoundingClientRect();
				const pill = trigger.getBoundingClientRect();
				return { x: width.left, y: pill.top - 4, width: width.width, height: pill.height + 4 };
			},
			anchorPosition: AnchorPosition.ABOVE,
			anchorAxisAlignment: AnchorAxisAlignment.VERTICAL,
			render: container => {
				const view = new WispAgentsPanelView(container, sections, this, hide, trigger);
				store.add(view);
				return view;
			},
			onHide: () => {
				hidden = true;
				store.dispose();
				onDidHide();
				if (trigger.isConnected) {
					trigger.focus();
				}
			},
		});
		// Clicking outside the card, other than on the pill, closes it; the pill closes it itself.
		const targetWindow = getWindow(trigger);
		store.add(addDisposableListener(targetWindow.document, EventType.MOUSE_DOWN, event => {
			const target = event.target as Node | null;
			if (target && !trigger.contains(target) && !(target instanceof targetWindow.Element && target.closest('.wisp-agents-panel'))) {
				hide();
			}
		}, true));
		// Closing the panel any way disposes this too.
		return store.add(toDisposable(hide));
	}
}

/** The card itself, rendered into the context view. */
class WispAgentsPanelView extends Disposable {

	private readonly list: HTMLElement;
	private readonly more: HTMLButtonElement;
	private rows: readonly IWispAgentRow[] = [];
	private rowElements: HTMLElement[] = [];
	private active = 0;
	private expanded = false;

	constructor(
		container: HTMLElement,
		sections: IObservable<readonly IChatPillSection[]>,
		private readonly panel: WispAgentsPanel,
		private readonly hide: () => void,
		trigger: HTMLElement,
	) {
		super();
		const width = composerOf(trigger).getBoundingClientRect().width;
		const title = localize('wispAgents.title', "Agents");
		const element = append(container, $('.wisp-agents-panel', { role: 'dialog', 'aria-label': title }));
		if (width) {
			element.style.width = `${Math.round(width)}px`;
		}
		const header = append(element, $('.wisp-agents-panel-header'));
		append(header, $('span.wisp-agents-panel-title', { id: 'wisp-agents-panel-title' }, title));
		const close = append(header, $('button.wisp-agents-panel-close', { type: 'button', 'aria-label': localize('wispAgents.close', "Close"), title: localize('wispAgents.close', "Close") }));
		append(close, $(`span${ThemeIcon.asCSSSelector(Codicon.close)}`, { 'aria-hidden': 'true' }));
		this._register(addDisposableListener(close, EventType.CLICK, () => this.hide()));

		this.list = append(element, $('.wisp-agents-panel-list', { role: 'listbox', tabindex: '0', 'aria-labelledby': 'wisp-agents-panel-title' }));
		this.more = append(element, $('button.wisp-agents-panel-more', { type: 'button' }, localize('wispAgents.more', "More"))) as HTMLButtonElement;
		this._register(addDisposableListener(this.more, EventType.CLICK, () => {
			this.expanded = true;
			this.render();
			this.focusRow(Math.min(AGENTS_PANEL_ROWS, this.rows.length - 1));
		}));

		this._register(addDisposableListener(element, EventType.KEY_DOWN, event => this.onKeyDown(event)));
		this._register(addDisposableListener(this.list, EventType.FOCUS, () => this.focusRow(this.active)));

		this._register(autorun(reader => {
			const entries = sections.read(reader).flatMap(section => section.entries);
			this.rows = entries.map(entry => this.panel.row(entry, reader));
			if (this.rows.length === 0) {
				this.hide();
				return;
			}
			this.render();
		}));
		this.focusRow(0);
	}

	private render(): void {
		const shown = this.expanded ? this.rows : this.rows.slice(0, AGENTS_PANEL_ROWS);
		const focused = this.list.ownerDocument.activeElement;
		const hadFocus = !!focused && (focused === this.list || this.list.contains(focused));
		this.active = Math.min(this.active, shown.length - 1);
		this.rowElements = shown.map((row, index) => this.renderRow(row, index));
		reset(this.list, ...this.rowElements);
		this.more.hidden = this.expanded || this.rows.length <= AGENTS_PANEL_ROWS;
		this.updateActive(hadFocus);
	}

	private renderRow(row: IWispAgentRow, index: number): HTMLElement {
		const element = $('.wisp-agents-panel-row', { role: 'option', id: `wisp-agents-panel-row-${index}`, 'aria-label': row.ariaLabel, 'aria-selected': 'false' });
		append(element, $(`span.wisp-agent-mark.wisp-agent-mark-${markOf(row)}`, { 'aria-hidden': 'true' }));
		append(element, $('span.wisp-agents-panel-row-title', { 'aria-hidden': 'true' }, row.title));
		if (row.location) {
			const where = append(element, $('span.wisp-agents-panel-row-where', { 'aria-hidden': 'true' }));
			append(where, $(`span${ThemeIcon.asCSSSelector(row.isLocal ? Codicon.deviceDesktop : Codicon.server)}`));
			append(where, $('span', undefined, row.location));
		}
		append(element, $('span.wisp-agents-panel-row-state', { 'aria-hidden': 'true' }, row.state?.label ?? ''));
		element.addEventListener('mousedown', event => event.preventDefault());
		element.addEventListener('click', () => {
			this.active = index;
			this.open(row);
		});
		return element;
	}

	private onKeyDown(event: KeyboardEvent): void {
		const inList = event.target === this.list || this.list.contains(event.target as Node);
		const stop = () => {
			event.preventDefault();
			event.stopPropagation();
		};
		switch (event.key) {
			case 'Escape':
				stop();
				this.hide();
				return;
			case 'ArrowDown':
			case 'ArrowUp':
			case 'Home':
			case 'End': {
				if (!inList) {
					return;
				}
				stop();
				const last = this.rowElements.length - 1;
				const next = event.key === 'Home' ? 0
					: event.key === 'End' ? last
						: event.key === 'ArrowDown' ? Math.min(last, this.active + 1)
							: Math.max(0, this.active - 1);
				this.focusRow(next);
				return;
			}
			case 'Enter':
			case ' ':
				if (inList && this.rows[this.active]) {
					stop();
					this.open(this.rows[this.active]);
				}
				return;
		}
	}

	private open(row: IWispAgentRow): void {
		this.hide();
		row.entry.open();
	}

	private focusRow(index: number): void {
		this.active = Math.max(0, index);
		this.updateActive(true);
		if (this.rowElements.length > 0) {
			this.list.focus();
		}
	}

	private updateActive(scroll: boolean): void {
		this.rowElements.forEach((element, index) => {
			const active = index === this.active;
			element.classList.toggle('active', active);
			element.setAttribute('aria-selected', String(active));
			if (active && scroll) {
				element.scrollIntoView({ block: 'nearest' });
			}
		});
		const active = this.rowElements[this.active];
		if (active) {
			this.list.setAttribute('aria-activedescendant', active.id);
		} else {
			this.list.removeAttribute('aria-activedescendant');
		}
	}
}

/** The composer the pill sits above, whose width the panel takes, as in the reference. */
function composerOf(trigger: HTMLElement): HTMLElement {
	const input = trigger.closest<HTMLElement>('.interactive-input-part');
	return input?.querySelector<HTMLElement>('.chat-input-container') ?? input ?? trigger.closest<HTMLElement>('.chat-pills-row') ?? trigger;
}

function markOf(row: IWispAgentRow): WispAgentMark {
	return row.state?.mark ?? 'hollow';
}

function parseRun(id: string): ReturnType<typeof agentRunOf> {
	try {
		return agentRunOf(URI.parse(id));
	} catch {
		return undefined;
	}
}

/**
 * Registers the panel for the subagents pill, and announces a subagent's state politely when it
 * changes, so a screen reader hears "Fix rounding: Failed" without opening the panel.
 */
export class WispAgentsPanelContribution extends Disposable implements IWorkbenchContribution {

	static readonly ID = 'sessions.contrib.wispAgentsPanel';

	constructor(
		@IWispAgentsService agentsService: IWispAgentsService,
		@IWispHostStatusService hostStatusService: IWispHostStatusService,
		@IContextViewService contextViewService: IContextViewService,
	) {
		super();
		this._register(registerChatPillPanel(SUBAGENTS_PILL_WIDGET_ID, new WispAgentsPanel(agentsService, hostStatusService, contextViewService)));
		this._register(new WispAgentStatusAnnouncer(agentsService));
	}
}

/** Announces changes of a run's state (not of its step), for the live-region requirement. */
export class WispAgentStatusAnnouncer extends Disposable {

	private readonly known = new Map<string, string>();

	constructor(
		agentsService: IWispAgentsService,
		private readonly announce: (message: string) => void = status,
	) {
		super();
		this._register(agentsService.onDidAddEvent(({ runId, event }) => {
			if (event.event.kind !== 'agent.updated' && event.event.kind !== 'agent.started' && event.event.kind !== 'agent.diffReady') {
				return;
			}
			const run = agentsService.getRun(runId);
			if (!run) {
				return;
			}
			const state = agentState(run);
			const key = `${state.mark}:${state.status}`;
			const previous = this.known.get(runId);
			this.known.set(runId, key);
			if (previous !== undefined && previous !== key) {
				this.announce(localize('wispAgents.announce', "{0}: {1}", agentTitle(run.prompt), state.running ? localize('wispAgents.running', "Running") : state.label));
			}
		}));
	}
}

registerWorkbenchContribution2(WispAgentsPanelContribution.ID, WispAgentsPanelContribution, WorkbenchPhase.BlockRestore);
