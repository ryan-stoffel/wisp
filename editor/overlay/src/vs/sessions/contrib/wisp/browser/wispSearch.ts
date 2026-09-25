/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { KeyCode, KeyMod } from '../../../../base/common/keyCodes.js';
import { ThemeIcon } from '../../../../base/common/themables.js';
import { URI } from '../../../../base/common/uri.js';
import { EditorContextKeys } from '../../../../editor/common/editorContextKeys.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ContextKeyExpr } from '../../../../platform/contextkey/common/contextkey.js';
import { ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { KeybindingWeight } from '../../../../platform/keybinding/common/keybindingsRegistry.js';
import { IQuickInputService, IQuickPickItem } from '../../../../platform/quickinput/common/quickInput.js';
import { IsSessionsWindowContext } from '../../../../workbench/common/contextkeys.js';
import { ChatContextKeys } from '../../../../workbench/contrib/chat/common/actions/chatContextKeys.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { ISession } from '../../../services/sessions/common/session.js';
import { ISessionsManagementService } from '../../../services/sessions/common/sessionsManagement.js';
import { WISP_SESSIONS_PROVIDER_ID } from '../../providers/wisp/browser/wispSessionsProvider.js';
import { compactAge, WISP_PROJECT_SESSION_TYPE } from '../../providers/wisp/common/wispProjects.js';

export const WISP_SEARCH_COMMAND = 'wisp.search';

interface ISessionPick extends IQuickPickItem {
	readonly resource: URI;
}

/** wisp's projects, newest activity first, as the sidebar lists them. */
export function projectSessions(sessionsManagementService: ISessionsManagementService): ISession[] {
	return sessionsManagementService.getSessions()
		.filter(session => session.providerId === WISP_SESSIONS_PROVIDER_ID && session.sessionType === WISP_PROJECT_SESSION_TYPE)
		.sort((a, b) => b.updatedAt.get().getTime() - a.updatedAt.get().getTime());
}

/** The Quick Pick's items: every project, newest activity first. Threads join them with #110. */
export function searchPicks(sessions: readonly ISession[], now: number): ISessionPick[] {
	return sessions.map(session => ({
		label: session.title.get(),
		iconClass: ThemeIcon.asClassName(session.icon),
		description: compactAge(session.updatedAt.get(), now),
		ariaLabel: localize('wispSearch.projectAria', "{0}, project", session.title.get()),
		resource: session.resource,
	}));
}

registerAction2(class SearchAction extends Action2 {
	constructor() {
		super({
			id: WISP_SEARCH_COMMAND,
			title: localize2('wispSearch.title', "Search Projects and Threads"),
			category: localize2('wisp.category', "Wisp"),
			f1: true,
			keybinding: {
				primary: KeyMod.CtrlCmd | KeyCode.KeyK,
				// Above the ⌘K chords, which stay available in text editors other than the chat input.
				weight: KeybindingWeight.WorkbenchContrib + 1,
				when: ContextKeyExpr.and(
					IsSessionsWindowContext,
					ContextKeyExpr.or(EditorContextKeys.editorTextFocus.negate(), ChatContextKeys.inChatInput),
				),
			},
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		const quickInputService = accessor.get(IQuickInputService);
		const sessionsService = accessor.get(ISessionsService);
		const picks = searchPicks(projectSessions(accessor.get(ISessionsManagementService)), Date.now());
		const picked = await quickInputService.pick<ISessionPick>(picks.length ? picks : [{
			label: localize('wispSearch.empty', "No projects yet"),
			resource: URI.from({ scheme: 'wisp-none', path: '/' }),
			disabled: true,
		}], {
			placeHolder: localize('wispSearch.placeholder', "Search projects and threads"),
			matchOnDescription: false,
		});
		if (picked && !picked.disabled) {
			await sessionsService.openSession(picked.resource);
		}
	}
});
