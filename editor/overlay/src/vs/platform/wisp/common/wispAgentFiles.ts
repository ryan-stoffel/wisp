/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { decodeBase64 } from '../../../base/common/buffer.js';
import { Event } from '../../../base/common/event.js';
import { Disposable, IDisposable } from '../../../base/common/lifecycle.js';
import { URI } from '../../../base/common/uri.js';
import { createFileSystemProviderError, FileSystemProviderCapabilities, FileSystemProviderErrorCode, FileType, IFileChange, IFileDeleteOptions, IFileOverwriteOptions, IFileSystemProviderWithFileReadWriteCapability, IFileWriteOptions, IStat, IWatchOptions } from '../../files/common/files.js';
import { IWispdService, WispdError } from './wispd.js';
import type { AgentDiffFile, AgentDiffResult, AgentFileResult, AgentFileSide, RunId } from './wispProtocol.js';

/**
 * The scheme of a file in an agent run's diff (#157): `wisp-agent://<runId>/<base|head>/<path>?<commit>`.
 * Every byte comes from wispd's `agent/file`, so a review works the same for a run on this Mac and
 * on a remote host, where the editor can't open the worktree (#67).
 */
export const WISP_AGENT_SCHEME = 'wisp-agent';

/** The scheme of one run's review, the multi-diff editor's source: `wisp-agent-review://<runId>/<head>`. */
export const WISP_AGENT_REVIEW_SCHEME = 'wisp-agent-review';

export interface IAgentFileRef {
	readonly runId: RunId;
	readonly side: AgentFileSide;
	/** Relative to the repository root, as `agent/diff` lists it. */
	readonly path: string;
	/**
	 * The commit this side was when the review opened. A read that finds another commit, because
	 * the run committed again, fails rather than mix two versions in one review.
	 */
	readonly commit: string;
}

export function agentFileUri(ref: IAgentFileRef): URI {
	return URI.from({
		scheme: WISP_AGENT_SCHEME,
		authority: ref.runId,
		path: `/${ref.side}/${ref.path}`,
		query: ref.commit,
	});
}

export function parseAgentFileUri(uri: URI): IAgentFileRef | undefined {
	if (uri.scheme !== WISP_AGENT_SCHEME || !uri.authority) {
		return undefined;
	}
	const match = /^\/(base|head)\/(.+)$/.exec(uri.path);
	if (!match) {
		return undefined;
	}
	return { runId: uri.authority as RunId, side: match[1] as AgentFileSide, path: match[2], commit: uri.query };
}

export function agentReviewUri(runId: RunId, head: string): URI {
	return URI.from({ scheme: WISP_AGENT_REVIEW_SCHEME, authority: runId, path: `/${head}` });
}

export function parseAgentReviewUri(uri: URI): { readonly runId: RunId; readonly head: string } | undefined {
	if (uri.scheme !== WISP_AGENT_REVIEW_SCHEME || !uri.authority || uri.path.length < 2) {
		return undefined;
	}
	return { runId: uri.authority as RunId, head: uri.path.slice(1) };
}

/**
 * The commit to send as `agent/accept`'s `commit`, so wispd refuses if the run committed after it
 * was reviewed: the one the caller names, else the head of the run's review open in the editor
 * (`activeReview`, the active multi-diff editor's source), else the run's latest commit, which the
 * accept confirmation describes.
 */
export function reviewedCommit(run: { readonly id: RunId; readonly diff?: { readonly commit: string } }, activeReview: URI | undefined, named?: string): string | undefined {
	if (named) {
		return named;
	}
	const review = activeReview && parseAgentReviewUri(activeReview);
	if (review && review.runId === run.id) {
		return review.head;
	}
	return run.diff?.commit;
}

export interface IAgentReviewItem {
	/** The base side; absent for an added file. */
	readonly original: URI | undefined;
	/** The head side; absent for a deleted file. */
	readonly modified: URI | undefined;
	readonly file: AgentDiffFile;
}

/** One diff editor entry per changed file: added files have no base side, deleted files no head. */
export function agentReviewItems(runId: RunId, diff: AgentDiffResult): IAgentReviewItem[] {
	return diff.files.map(file => {
		const basePath = file.oldPath ?? file.path;
		const original = file.status === 'added'
			? undefined
			: agentFileUri({ runId, side: 'base', path: basePath, commit: diff.base });
		const modified = file.status === 'deleted'
			? undefined
			: agentFileUri({ runId, side: 'head', path: file.path, commit: diff.head });
		return { original, modified, file };
	});
}

/**
 * A read-only file system over `agent/file`. It answers only `stat` and `readFile`: a run's files
 * are commits, which never change, so there is nothing to watch or list.
 */
export class WispAgentFileSystemProvider extends Disposable implements IFileSystemProviderWithFileReadWriteCapability {

	readonly capabilities = FileSystemProviderCapabilities.FileReadWrite
		| FileSystemProviderCapabilities.Readonly
		| FileSystemProviderCapabilities.PathCaseSensitive;

	readonly onDidChangeCapabilities = Event.None;
	readonly onDidChangeFile: Event<readonly IFileChange[]> = Event.None;

	constructor(private readonly wispdService: IWispdService) {
		super();
	}

	watch(_resource: URI, _opts: IWatchOptions): IDisposable {
		return Disposable.None;
	}

	/** Asks wispd for the file's size only (`sizeOnly`), so a `stat` never downloads the file. */
	async stat(resource: URI): Promise<IStat> {
		const { result } = await this.fetch(resource, true);
		return { type: FileType.File, ctime: 0, mtime: 0, size: result.size ?? 0 };
	}

	async readFile(resource: URI): Promise<Uint8Array> {
		const { ref, result } = await this.fetch(resource, false);
		if (result.tooLarge || result.content === undefined) {
			throw createFileSystemProviderError(`${ref.path} is too large to review here (${result.size ?? 0} bytes)`, FileSystemProviderErrorCode.FileTooLarge);
		}
		return decodeBase64(result.content).buffer;
	}

	/** One `agent/file` call, with every answer that isn't a file on the review's commit as an error. */
	private async fetch(resource: URI, sizeOnly: boolean): Promise<{ readonly ref: IAgentFileRef; readonly result: AgentFileResult }> {
		const ref = parseAgentFileUri(resource);
		if (!ref) {
			throw createFileSystemProviderError(`${resource.toString()} is not an agent run's file`, FileSystemProviderErrorCode.FileNotFound);
		}
		let result: AgentFileResult;
		try {
			result = await this.wispdService.request('agent/file', sizeOnly
				? { runId: ref.runId, path: ref.path, side: ref.side, sizeOnly: true }
				: { runId: ref.runId, path: ref.path, side: ref.side });
		} catch (error) {
			if (error instanceof WispdError && (error.kind === 'runNotFound' || error.kind === 'runAccepted')) {
				throw createFileSystemProviderError(error.message, FileSystemProviderErrorCode.FileNotFound);
			}
			throw createFileSystemProviderError(error instanceof Error ? error : String(error), FileSystemProviderErrorCode.Unavailable);
		}
		if (ref.commit && result.commit !== ref.commit) {
			throw createFileSystemProviderError(`The run has new changes since this review opened; review it again`, FileSystemProviderErrorCode.Unavailable);
		}
		if (!result.exists) {
			throw createFileSystemProviderError(`${ref.path} does not exist on the ${ref.side} side`, FileSystemProviderErrorCode.FileNotFound);
		}
		return { ref, result };
	}

	async writeFile(resource: URI, _content: Uint8Array, _opts: IFileWriteOptions): Promise<void> {
		throw this.readOnly(resource);
	}

	async mkdir(resource: URI): Promise<void> {
		throw this.readOnly(resource);
	}

	async readdir(_resource: URI): Promise<[string, FileType][]> {
		return [];
	}

	async delete(resource: URI, _opts: IFileDeleteOptions): Promise<void> {
		throw this.readOnly(resource);
	}

	async rename(from: URI, _to: URI, _opts: IFileOverwriteOptions): Promise<void> {
		throw this.readOnly(from);
	}

	private readOnly(resource: URI): Error {
		return createFileSystemProviderError(`${resource.toString()} is part of an agent run's diff, which is read-only`, FileSystemProviderErrorCode.NoPermissions);
	}
}
