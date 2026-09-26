/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { $, addDisposableListener, append, clearNode, EventType, getWindow } from '../../../../base/browser/dom.js';
import { StandardKeyboardEvent } from '../../../../base/browser/keyboardEvent.js';
import { toAction } from '../../../../base/common/actions.js';
import { Codicon } from '../../../../base/common/codicons.js';
import { toErrorMessage } from '../../../../base/common/errorMessage.js';
import { fromNow } from '../../../../base/common/date.js';
import { KeyCode, KeyMod } from '../../../../base/common/keyCodes.js';
import { Disposable, DisposableStore, toDisposable } from '../../../../base/common/lifecycle.js';
import { autorun, observableSignal, observableSignalFromEvent } from '../../../../base/common/observable.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { localize } from '../../../../nls.js';
import { IContextMenuService } from '../../../../platform/contextview/browser/contextView.js';
import { IDialogService } from '../../../../platform/dialogs/common/dialogs.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { ISession, SessionStatus } from '../../../services/sessions/common/session.js';
import { ISessionsManagementService } from '../../../services/sessions/common/sessionsManagement.js';
import { WISP_SESSIONS_PROVIDER_ID } from '../../providers/wisp/browser/wispSessionsProvider.js';
import { compactAge } from '../../providers/wisp/common/wispProjects.js';
import { placeThreadSessions } from '../../providers/wisp/common/wispThreads.js';

/** How often the rows' ages are refreshed. */
const AGE_REFRESH_MS = 60_000;

/**
 * The Repositories and No Repo sections of wisp's sidebar (decision record 0011,
 * docs/design/agents-window.md, note 3): normal threads grouped under their repository, and
 * quick chats under No Repo. Each row shows the thread's title and a relative age; its context
 * menu archives or deletes it through the provider. Both sections stay hidden while empty.
 */
export class WispThreadSections extends Disposable {

	constructor(
		parent: HTMLElement,
		idPrefix: string,
		@ISessionsManagementService private readonly sessionsManagementService: ISessionsManagementService,
		@ISessionsService private readonly sessionsService: ISessionsService,
		@IContextMenuService private readonly contextMenuService: IContextMenuService,
		@IDialogService private readonly dialogService: IDialogService,
		@INotificationService private readonly notificationService: INotificationService,
	) {
		super();
		const repositories = this.renderSection(parent, `${idPrefix}-repositories`, localize('wispThreads.repositories', "Repositories"));
		const noRepo = this.renderSection(parent, `${idPrefix}-no-repo`, localize('wispThreads.noRepo', "No Repo"));
		const rowStore = this._register(new DisposableStore());

		const sessionsChanged = observableSignalFromEvent(this, this.sessionsManagementService.onDidChangeSessions);
		const tick = observableSignal(this);
		const interval = getWindow(parent).setInterval(() => tick.trigger(undefined), AGE_REFRESH_MS);
		this._register(toDisposable(() => getWindow(parent).clearInterval(interval)));

		this._register(autorun(reader => {
			sessionsChanged.read(reader);
			tick.read(reader);
			const sessions = this.sessionsManagementService.getSessions().filter(session => session.providerId === WISP_SESSIONS_PROVIDER_ID);
			for (const session of sessions) {
				session.title.read(reader);
				session.updatedAt.read(reader);
				session.status.read(reader);
				session.isArchived.read(reader);
			}
			const active = this.sessionsService.activeSession.read(reader)?.resource.toString();
			const placement = placeThreadSessions(sessions);
			const focused = parent.ownerDocument.activeElement instanceof HTMLElement && parent.contains(parent.ownerDocument.activeElement)
				? parent.ownerDocument.activeElement.dataset.session
				: undefined;
			rowStore.clear();
			const now = Date.now();

			clearNode(repositories.list);
			for (const group of placement.repositories) {
				const item = append(repositories.list, $('li.wisp-threads-repo'));
				const headingId = `${idPrefix}-repo-${repositories.list.childElementCount}`;
				const heading = append(item, $('.wisp-threads-repo-name', { id: headingId }));
				append(heading, $(`span${ThemeIcon.asCSSSelector(Codicon.repo)}`, { 'aria-hidden': 'true' }));
				append(heading, $('span', undefined, group.label));
				const rows = append(item, $('ul.wisp-threads-rows', { 'aria-labelledby': headingId }));
				for (const session of group.sessions) {
					this.renderRow(rows, session, now, active, group.label, rowStore);
				}
			}
			repositories.section.hidden = placement.repositories.length === 0;

			clearNode(noRepo.list);
			for (const session of placement.noRepo) {
				this.renderRow(noRepo.list, session, now, active, undefined, rowStore);
			}
			noRepo.section.hidden = placement.noRepo.length === 0;

			this.rovingTabStops(repositories.section, focused);
			this.rovingTabStops(noRepo.section, focused);
		}));

		for (const { section } of [repositories, noRepo]) {
			this._register(addDisposableListener(section, EventType.KEY_DOWN, event => this.onKeyDown(section, event)));
		}
	}

	private renderSection(parent: HTMLElement, id: string, title: string): { section: HTMLElement; list: HTMLElement } {
		const section = append(parent, $('section.wisp-threads-section.wisp-threads-thread-section', { 'aria-labelledby': id }));
		section.hidden = true;
		const header = append(section, $('.wisp-threads-section-header'));
		append(header, $('h2.wisp-threads-section-title', { id }, title));
		const list = append(section, $('ul.wisp-threads-rows', { 'aria-labelledby': id }));
		return { section, list };
	}

	private renderRow(list: HTMLElement, session: ISession, now: number, active: string | undefined, repo: string | undefined, store: DisposableStore): HTMLButtonElement {
		const title = session.title.get();
		const updated = session.updatedAt.get();
		const running = session.status.get() === SessionStatus.InProgress;
		const item = append(list, $('li'));
		const row = append(item, $<HTMLButtonElement>('button.wisp-threads-row.wisp-threads-thread-row', { type: 'button' }));
		row.dataset.session = session.resource.toString();
		row.tabIndex = -1;
		const age = fromNow(updated, true);
		row.setAttribute('aria-label', repo
			? localize('wispThreads.threadRowLabel', "{0}, chat in {1}, updated {2}", title, repo, age)
			: localize('wispThreads.quickChatRowLabel', "{0}, chat, updated {1}", title, age));
		row.setAttribute('aria-haspopup', 'menu');
		if (active === session.resource.toString()) {
			row.classList.add('selected');
			row.setAttribute('aria-current', 'true');
		}
		const mark = append(row, $(`span.wisp-threads-thread-mark${ThemeIcon.asCSSSelector(running ? Codicon.circleFilled : Codicon.commentDiscussion)}`, { 'aria-hidden': 'true' }));
		mark.classList.toggle('running', running);
		append(row, $('span.wisp-threads-row-title', { 'aria-hidden': 'true' }, title));
		append(row, $('span.wisp-threads-row-age', { 'aria-hidden': 'true' }, compactAge(updated, now)));
		store.add(addDisposableListener(row, EventType.CLICK, () => this.sessionsService.openSession(session.resource)));
		store.add(addDisposableListener(row, EventType.CONTEXT_MENU, event => {
			event.preventDefault();
			this.showMenu(row, session, { x: event.clientX, y: event.clientY });
		}));
		return row;
	}

	/** One tab stop per section: the row last focused, else the open thread, else the first. */
	private rovingTabStops(section: HTMLElement, focused: string | undefined): void {
		const rows = [...section.querySelectorAll<HTMLButtonElement>('button.wisp-threads-row')];
		const stop = rows.find(row => row.dataset.session === focused)
			?? rows.find(row => row.classList.contains('selected'))
			?? rows[0];
		for (const row of rows) {
			row.tabIndex = row === stop ? 0 : -1;
		}
		if (focused !== undefined && stop?.dataset.session === focused) {
			stop.focus();
		}
	}

	private onKeyDown(section: HTMLElement, browserEvent: KeyboardEvent): void {
		const rows = [...section.querySelectorAll<HTMLButtonElement>('button.wisp-threads-row')];
		const current = rows.indexOf(browserEvent.target as HTMLButtonElement);
		if (current === -1) {
			return;
		}
		const event = new StandardKeyboardEvent(browserEvent);
		if (event.equals(KeyMod.Shift | KeyCode.F10) || event.keyCode === KeyCode.ContextMenu) {
			event.preventDefault();
			const session = this.sessionsManagementService.getSessions().find(candidate => candidate.resource.toString() === rows[current].dataset.session);
			if (session) {
				this.showMenu(rows[current], session, rows[current]);
			}
			return;
		}
		let next: number;
		switch (event.keyCode) {
			case KeyCode.DownArrow: next = Math.min(current + 1, rows.length - 1); break;
			case KeyCode.UpArrow: next = Math.max(current - 1, 0); break;
			case KeyCode.Home: next = 0; break;
			case KeyCode.End: next = rows.length - 1; break;
			default: return;
		}
		event.preventDefault();
		rows[current].tabIndex = -1;
		rows[next].tabIndex = 0;
		rows[next].focus();
	}

	private showMenu(row: HTMLElement, session: ISession, anchor: HTMLElement | { x: number; y: number }): void {
		this.contextMenuService.showContextMenu({
			getAnchor: () => anchor,
			getActions: () => [
				toAction({
					id: 'wisp.thread.archive',
					label: localize('wispThreads.archive', "Archive"),
					run: () => this.run(() => this.sessionsManagementService.archiveSession(session), localize('wispThreads.archiveFailed', "Couldn't archive the chat")),
				}),
				toAction({
					id: 'wisp.thread.delete',
					label: localize('wispThreads.delete', "Delete..."),
					run: () => this.delete(session),
				}),
			],
			onHide: () => row.focus(),
		});
	}

	private async delete(session: ISession): Promise<void> {
		const { confirmed } = await this.dialogService.confirm({
			message: localize('wispThreads.deleteConfirm', "Delete \"{0}\"?", session.title.get()),
			detail: localize('wispThreads.deleteDetail', "wispd stops the agent if it is running, and removes its worktree, its branch, and its transcript. This can't be undone."),
			primaryButton: localize('wispThreads.deleteButton', "&&Delete"),
		});
		if (confirmed) {
			await this.run(() => this.sessionsManagementService.deleteSession(session), localize('wispThreads.deleteFailed', "Couldn't delete the chat"));
		}
	}

	private async run(action: () => Promise<void>, failure: string): Promise<void> {
		try {
			await action();
		} catch (error) {
			this.notificationService.error(`${failure}: ${toErrorMessage(error)}`);
		}
	}
}
