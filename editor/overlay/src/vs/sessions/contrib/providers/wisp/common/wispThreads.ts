/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { URI } from '../../../../../base/common/uri.js';
import type { RunId } from '../../../../../platform/wisp/common/wispProtocol.js';
import { ISession } from '../../../../services/sessions/common/session.js';
import { agentChatResource } from './wispAgentRuns.js';

/**
 * A normal thread's session type (decision records 0011 and 0017): one agent with no coordinator,
 * under its repo in Repositories, or under No Repo.
 */
export const WISP_THREAD_SESSION_TYPE = 'wisp.thread';

/**
 * The scheme of a repository on a host other than this Mac. v1 can't open a host's folders (#67),
 * so such a workspace only names the repository; on this Mac a thread's workspace is a file URI.
 */
export const WISP_REPO_SCHEME = 'wisp-repo';

/** The capability that gates `thread/*`, `repo/*`, and their events (decision record 0017). */
export const THREADS_CAPABILITY = 'threads';

/** The segment in a thread's chat resource where a subagent's has its project. */
const THREAD_SEGMENT = 'thread';

export function threadResource(runId: RunId): URI {
	return URI.from({ scheme: WISP_THREAD_SESSION_TYPE, path: `/${runId}` });
}

/** The run in a thread's session resource, or `undefined` for any other resource. */
export function threadRunOf(resource: URI): RunId | undefined {
	if (resource.scheme !== WISP_THREAD_SESSION_TYPE) {
		return undefined;
	}
	return /^\/([^/]+)$/.exec(resource.path)?.[1];
}

/**
 * A thread's main chat: a `wisp.agent` chat, so #105's transcript, composer, and Stop serve it.
 * It doesn't name the repo entry, which a draft doesn't know yet.
 */
export function threadChatResource(runId: RunId): URI {
	return agentChatResource(THREAD_SEGMENT, runId);
}

/** A repository's workspace URI: a file on this Mac, or a `wisp-repo:` path on another host. */
export function repoUri(path: string, isLocal: boolean): URI {
	return isLocal ? URI.file(path) : URI.from({ scheme: WISP_REPO_SCHEME, path });
}

/** The host path a workspace URI names, or `undefined` for a URI wisp doesn't own. */
export function repoPathOf(uri: URI, isLocal: boolean): string | undefined {
	if (uri.scheme === WISP_REPO_SCHEME || (isLocal && uri.scheme === 'file')) {
		return uri.path;
	}
	return undefined;
}

/** One repository's threads, as the sidebar's Repositories section lists them. */
export interface IWispRepoThreads {
	/** The workspace URI, which identifies the repository. */
	readonly key: string;
	readonly label: string;
	readonly sessions: readonly ISession[];
}

/** Where the sidebar lists threads: under their repository, or under No Repo. */
export interface IWispThreadPlacement {
	readonly repositories: readonly IWispRepoThreads[];
	readonly noRepo: readonly ISession[];
}

/**
 * Places the provider's `wisp.thread` sessions for the sidebar (decision record 0011): a thread
 * with a workspace goes under that repository in Repositories, and a quick chat under No Repo.
 * Archived threads are left out. Repositories are in name order, and threads newest first.
 */
export function placeThreadSessions(sessions: readonly ISession[]): IWispThreadPlacement {
	const repositories = new Map<string, { label: string; sessions: ISession[] }>();
	const noRepo: ISession[] = [];
	for (const session of sessions) {
		if (session.sessionType !== WISP_THREAD_SESSION_TYPE || session.isArchived.get()) {
			continue;
		}
		const workspace = session.workspace.get();
		if (!workspace || session.isQuickChat?.get()) {
			noRepo.push(session);
			continue;
		}
		const key = workspace.uri.toString();
		let group = repositories.get(key);
		if (!group) {
			group = { label: workspace.label, sessions: [] };
			repositories.set(key, group);
		}
		group.sessions.push(session);
	}
	const newestFirst = (list: readonly ISession[]) => [...list].sort((a, b) => b.updatedAt.get().getTime() - a.updatedAt.get().getTime());
	return {
		repositories: [...repositories].map(([key, group]) => ({ key, label: group.label, sessions: newestFirst(group.sessions) }))
			.sort((a, b) => a.label.localeCompare(b.label) || a.key.localeCompare(b.key)),
		noRepo: newestFirst(noRepo),
	};
}
