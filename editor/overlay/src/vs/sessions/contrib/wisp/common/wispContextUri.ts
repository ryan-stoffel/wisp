/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { localize } from '../../../../nls.js';
import { URI } from '../../../../base/common/uri.js';
import type { ProjectId } from '../../../../platform/wisp/common/wispProtocol.js';

/** The scheme for a project's shared context folder (decision record 0005, note 10). */
export const WISP_CONTEXT_SCHEME = 'wisp-context';

/** Who the editor's own writes are attributed to in `context/write` and `context/list`. */
export const WISP_CONTEXT_EDITOR_WRITER = 'editor';

/** How `lastWriter` reads in the UI: the editor's own writes as "you", anything else as wispd sent it (agent attribution is #216). */
export function displayWriter(writer: string | undefined): string | undefined {
	return writer === WISP_CONTEXT_EDITOR_WRITER ? localize('wispContext.writerYou', "you") : writer;
}

/** wispd's limit on a shared context file's name (0005, #155). */
const MAX_NAME_BYTES = 255;
const EXTENSIONS = ['.md', '.markdown', '.txt'];
const encoder = new TextEncoder();

/**
 * wispd only ever holds Markdown or plain text in a project's shared context: one file name, no
 * path, no leading dot, no NUL, and one of the extensions it accepts. Shared by the add-file flow,
 * which asks a person, and {@link parseContextUri}, which trusts nothing a link or a stray URI hands
 * it: neither may reach `context/read` or `context/write` with a `..`, a subfolder, or an encoded
 * separator (`uri.path` is already percent-decoded by the time either sees it).
 */
export function validateContextFileName(name: string): string | undefined {
	if (name.length === 0) {
		return localize('wispContext.nameEmpty', "Enter a file name.");
	}
	if (name.includes('/') || name.includes('\\')) {
		return localize('wispContext.nameSlash', "Enter a file name, not a path.");
	}
	if (name.includes('\0')) {
		return localize('wispContext.nameNul', "Enter a name without NUL characters.");
	}
	if (name.startsWith('.')) {
		return localize('wispContext.nameDot', "Enter a name that doesn't start with a dot.");
	}
	if (!EXTENSIONS.some(extension => name.endsWith(extension))) {
		return localize('wispContext.nameExtension', "Enter a name ending in .md, .markdown, or .txt.");
	}
	if (encoder.encode(name).length > MAX_NAME_BYTES) {
		return localize('wispContext.nameLength', "Enter a name of at most {0} bytes.", MAX_NAME_BYTES);
	}
	return undefined;
}

/** A shared context file's URI: the project is the authority, the path is relative to its folder. */
export function toContextUri(project: ProjectId, path: string): URI {
	return URI.from({ scheme: WISP_CONTEXT_SCHEME, authority: project, path: `/${path}` });
}

export interface IParsedContextUri {
	readonly project: ProjectId;
	/** Empty for the project's shared context folder itself, which has no subdirectories. */
	readonly path: string;
}

/**
 * The reverse of {@link toContextUri}, for a URI whose path is the root or exactly one name that
 * passes {@link validateContextFileName}; `undefined` for anything else, including a URI that
 * isn't one of ours. Nothing gets past this into `context/read`, `context/write`, or the banner
 * with a `..`, a subfolder, or an encoded separator, whether wispd would have refused it too or not.
 */
export function parseContextUri(uri: URI): IParsedContextUri | undefined {
	if (uri.scheme !== WISP_CONTEXT_SCHEME || !uri.authority) {
		return undefined;
	}
	const path = uri.path.replace(/^\/+/, '');
	if (path !== '' && validateContextFileName(path) !== undefined) {
		return undefined;
	}
	return { project: uri.authority, path };
}
