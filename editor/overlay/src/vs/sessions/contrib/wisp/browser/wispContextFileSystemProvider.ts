/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { VSBuffer } from '../../../../base/common/buffer.js';
import { Emitter } from '../../../../base/common/event.js';
import { Disposable, IDisposable } from '../../../../base/common/lifecycle.js';
import { autorun } from '../../../../base/common/observable.js';
import { URI } from '../../../../base/common/uri.js';
import {
	createFileSystemProviderError, FileChangeType, FileSystemProviderCapabilities, FileSystemProviderErrorCode,
	FileType, IFileChange, IFileDeleteOptions, IFileOverwriteOptions, IFileSystemProviderWithFileReadWriteCapability,
	IFileWriteOptions, IStat, IWatchOptions,
} from '../../../../platform/files/common/files.js';
import { WispdError, WispdUnavailableError } from '../../../../platform/wisp/common/wispd.js';
import { ErrorCodes } from '../../../../platform/wisp/common/wispProtocol.js';
import type { ContextFile } from '../../../../platform/wisp/common/wispProtocol.js';
import { parseContextUri } from '../common/wispContextUri.js';
import { IWispContextService } from './wispContextService.js';

function toStat(file: ContextFile): IStat {
	const mtime = Date.parse(file.modifiedAt);
	return { type: FileType.File, ctime: mtime, mtime, size: file.size };
}

/**
 * Maps a failure from `IWispContextService` to what `IFileService` expects from a provider.
 * `contextTooLarge` covers wispd's two caps, per file and per project; `FileTooLarge` names only
 * the first, but wispd's own message tells them apart.
 */
function toProviderError(error: unknown): Error {
	if (error instanceof WispdError) {
		const code = error.kind === 'contextNotFound' || error.kind === 'projectNotFound' ? FileSystemProviderErrorCode.FileNotFound
			: error.kind === 'contextTooLarge' ? FileSystemProviderErrorCode.FileTooLarge
				: error.code === ErrorCodes.InvalidParams ? FileSystemProviderErrorCode.NoPermissions
					: FileSystemProviderErrorCode.Unknown;
		return createFileSystemProviderError(error.message, code);
	}
	if (error instanceof WispdUnavailableError) {
		return createFileSystemProviderError(error.message, FileSystemProviderErrorCode.Unavailable);
	}
	return createFileSystemProviderError(error instanceof Error ? error : String(error), FileSystemProviderErrorCode.Unknown);
}

/**
 * Serves the `wisp-context:` scheme (docs/design/agents-window.md, note 10; decision record
 * 0005): a shared context file opens through `context/read` and saves through `context/write`,
 * and `IWispContextService`'s `context.changed` events turn into `onDidChangeFile`. There are no
 * subdirectories and wispd never accepts a rename, copy, or delete over the protocol, so this
 * provider doesn't offer them either.
 */
export class WispContextFileSystemProvider extends Disposable implements IFileSystemProviderWithFileReadWriteCapability {

	readonly capabilities = FileSystemProviderCapabilities.FileReadWrite | FileSystemProviderCapabilities.PathCaseSensitive;

	private readonly _onDidChangeCapabilities = this._register(new Emitter<void>());
	readonly onDidChangeCapabilities = this._onDidChangeCapabilities.event;

	private readonly _onDidChangeFile = this._register(new Emitter<readonly IFileChange[]>());
	readonly onDidChangeFile = this._onDidChangeFile.event;

	constructor(@IWispContextService private readonly contextService: IWispContextService) {
		super();
	}

	watch(resource: URI, _opts: IWatchOptions): IDisposable {
		const parsed = parseContextUri(resource);
		if (!parsed?.path) {
			return Disposable.None;
		}
		const { project, path } = parsed;
		let seen: ContextFile | undefined;
		let initialized = false;
		return autorun(reader => {
			const state = this.contextService.state(project).read(reader);
			// Not loaded yet (or between a resync and its answer): nothing to compare against.
			if (state.kind !== 'ready') {
				return;
			}
			const file = state.files.find(candidate => candidate.path === path);
			if (!initialized) {
				// The list settling for the first time is not itself a change.
				initialized = true;
				seen = file;
				return;
			}
			if (JSON.stringify(file) === JSON.stringify(seen)) {
				return;
			}
			seen = file;
			this._onDidChangeFile.fire([{ resource, type: file ? FileChangeType.UPDATED : FileChangeType.DELETED }]);
		});
	}

	async stat(resource: URI): Promise<IStat> {
		const parsed = parseContextUri(resource);
		if (!parsed) {
			throw createFileSystemProviderError('not a shared context resource', FileSystemProviderErrorCode.FileNotFound);
		}
		if (!parsed.path) {
			return { type: FileType.Directory, ctime: 0, mtime: 0, size: 0 };
		}
		const state = this.contextService.state(parsed.project).get();
		const known = state.kind === 'ready' ? state.files.find(file => file.path === parsed.path) : undefined;
		if (known) {
			return toStat(known);
		}
		try {
			const { file } = await this.contextService.read(parsed.project, parsed.path);
			return toStat(file);
		} catch (error) {
			throw toProviderError(error);
		}
	}

	async readdir(resource: URI): Promise<[string, FileType][]> {
		const parsed = parseContextUri(resource);
		if (!parsed) {
			return [];
		}
		const state = this.contextService.state(parsed.project).get();
		const files = state.kind === 'ready' ? state.files : [];
		return files.map(file => [file.path, FileType.File]);
	}

	async readFile(resource: URI): Promise<Uint8Array> {
		const parsed = parseContextUri(resource);
		if (!parsed) {
			throw createFileSystemProviderError('not a shared context resource', FileSystemProviderErrorCode.FileNotFound);
		}
		if (!parsed.path) {
			throw createFileSystemProviderError('shared context has no subdirectories', FileSystemProviderErrorCode.FileIsADirectory);
		}
		try {
			const { content } = await this.contextService.read(parsed.project, parsed.path);
			return VSBuffer.fromString(content).buffer;
		} catch (error) {
			throw toProviderError(error);
		}
	}

	async writeFile(resource: URI, content: Uint8Array, _opts: IFileWriteOptions): Promise<void> {
		const parsed = parseContextUri(resource);
		if (!parsed) {
			throw createFileSystemProviderError('not a shared context resource', FileSystemProviderErrorCode.FileNotFound);
		}
		if (!parsed.path) {
			throw createFileSystemProviderError('shared context has no subdirectories', FileSystemProviderErrorCode.FileIsADirectory);
		}
		try {
			await this.contextService.write(parsed.project, parsed.path, VSBuffer.wrap(content).toString());
		} catch (error) {
			throw toProviderError(error);
		}
	}

	async mkdir(_resource: URI): Promise<void> {
		throw createFileSystemProviderError('shared context has no subdirectories', FileSystemProviderErrorCode.NoPermissions);
	}

	async delete(_resource: URI, _opts: IFileDeleteOptions): Promise<void> {
		throw createFileSystemProviderError('wispd does not support deleting shared context files', FileSystemProviderErrorCode.NoPermissions);
	}

	async rename(_from: URI, _to: URI, _opts: IFileOverwriteOptions): Promise<void> {
		throw createFileSystemProviderError('wispd does not support renaming shared context files', FileSystemProviderErrorCode.NoPermissions);
	}
}
