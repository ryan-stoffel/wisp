/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { URI } from '../../../../../base/common/uri.js';
import { localize } from '../../../../../nls.js';
import type { Project } from '../../../../../platform/wisp/common/wispProtocol.js';

/**
 * A project's session type, and the scheme of its session and of its coordinator chat
 * (decision record 0011): one session per project, whose one chat is the coordinator.
 */
export const WISP_PROJECT_SESSION_TYPE = 'wisp.project';

export function projectResource(id: string): URI {
	return URI.from({ scheme: WISP_PROJECT_SESSION_TYPE, path: `/${id}` });
}

/** The project id in a project's session or chat resource, or `undefined` for any other resource. */
export function projectIdOf(resource: URI): string | undefined {
	if (resource.scheme !== WISP_PROJECT_SESSION_TYPE) {
		return undefined;
	}
	const id = resource.path.replace(/^\//, '');
	return id.length > 0 && !id.includes('/') ? id : undefined;
}

/** The theme colors a project's glyph takes, picked by its id so a project keeps its color. */
export const PROJECT_GLYPH_COLORS = ['blue', 'green', 'yellow', 'purple', 'orange', 'red'] as const;
export type ProjectGlyphColor = typeof PROJECT_GLYPH_COLORS[number];

export interface IProjectGlyph {
	readonly letter: string;
	readonly color: ProjectGlyphColor;
}

export function projectGlyph(project: Pick<Project, 'id' | 'name'>): IProjectGlyph {
	const first = /[\p{L}\p{N}]/u.exec(project.name)?.[0];
	let hash = 0;
	for (let i = 0; i < project.id.length; i++) {
		hash = (hash * 31 + project.id.charCodeAt(i)) >>> 0;
	}
	return {
		letter: first ? first.toLocaleUpperCase() : '#',
		color: PROJECT_GLYPH_COLORS[hash % PROJECT_GLYPH_COLORS.length],
	};
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/** A short age for a sidebar row, like Cursor's: `now`, `2m`, `5h`, `3d`, `2w`, `4mo`, `1y`. */
export function compactAge(from: Date, now: number): string {
	const elapsed = Math.max(0, now - from.getTime());
	if (elapsed < MINUTE) {
		return localize('wispAge.now', "now");
	}
	if (elapsed < HOUR) {
		return localize('wispAge.minutes', "{0}m", Math.floor(elapsed / MINUTE));
	}
	if (elapsed < DAY) {
		return localize('wispAge.hours', "{0}h", Math.floor(elapsed / HOUR));
	}
	if (elapsed < WEEK) {
		return localize('wispAge.days', "{0}d", Math.floor(elapsed / DAY));
	}
	if (elapsed < 30 * DAY) {
		return localize('wispAge.weeks', "{0}w", Math.floor(elapsed / WEEK));
	}
	if (elapsed < 365 * DAY) {
		return localize('wispAge.months', "{0}mo", Math.floor(elapsed / (30 * DAY)));
	}
	return localize('wispAge.years', "{0}y", Math.floor(elapsed / (365 * DAY)));
}

/** A path with the home folder shown as `~`, for display only. */
export function tildify(path: string, home: string | undefined): string {
	if (!home) {
		return path;
	}
	const trimmed = home.replace(/\/+$/, '');
	if (path === trimmed) {
		return '~';
	}
	return trimmed && path.startsWith(`${trimmed}/`) ? `~${path.slice(trimmed.length)}` : path;
}
