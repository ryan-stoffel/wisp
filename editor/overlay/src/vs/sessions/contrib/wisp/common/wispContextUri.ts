/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { URI } from '../../../../base/common/uri.js';
import type { ProjectId } from '../../../../platform/wisp/common/wispProtocol.js';

/** The scheme for a project's shared context folder (decision record 0005, note 10). */
export const WISP_CONTEXT_SCHEME = 'wisp-context';

/** Who the editor's own writes are attributed to in `context/write` and `context/list`. */
export const WISP_CONTEXT_EDITOR_WRITER = 'editor';

/** A shared context file's URI: the project is the authority, the path is relative to its folder. */
export function toContextUri(project: ProjectId, path: string): URI {
	return URI.from({ scheme: WISP_CONTEXT_SCHEME, authority: project, path: `/${path}` });
}

export interface IParsedContextUri {
	readonly project: ProjectId;
	/** Empty for the project's shared context folder itself, which has no subdirectories. */
	readonly path: string;
}

/** The reverse of {@link toContextUri}, or `undefined` for a URI that isn't one of ours. */
export function parseContextUri(uri: URI): IParsedContextUri | undefined {
	if (uri.scheme !== WISP_CONTEXT_SCHEME || !uri.authority) {
		return undefined;
	}
	return { project: uri.authority, path: uri.path.replace(/^\/+/, '') };
}
