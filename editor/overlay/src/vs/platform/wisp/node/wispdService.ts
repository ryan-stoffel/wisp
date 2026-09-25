/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { existsSync } from 'fs';
import { join } from '../../../base/common/path.js';
import { CancellationToken } from '../../../base/common/cancellation.js';
import { Emitter, Event } from '../../../base/common/event.js';
import { Disposable, IDisposable } from '../../../base/common/lifecycle.js';
import { joinPath } from '../../../base/common/resources.js';
import { IConfigurationService } from '../../configuration/common/configuration.js';
import { INativeEnvironmentService } from '../../environment/common/environment.js';
import { ISharedProcessLifecycleService } from '../../lifecycle/node/sharedProcessLifecycleService.js';
import { ILogger, ILoggerService } from '../../log/common/log.js';
import { INativeHostService } from '../../native/common/native.js';
import { IProductService } from '../../product/common/productService.js';
import { IWispdService, IWispdSubscribeOptions, WispdMethod, WispdState, WispdSubscriptionMessage } from '../common/wispd.js';
import { IWispdTransportFactory, WispdClient } from '../common/wispdClient.js';
import { WISP_HOST_LOCAL, WISP_HOST_SETTING, WISP_REMOTE_WISPD_PATH_SETTING } from '../common/wispdConfiguration.js';
import { WispRequests } from '../common/wispProtocol.js';
import { DEFAULT_REMOTE_WISPD_CANDIDATES, WispdInvalidHostTransportFactory, WispdSshTransportFactory, validateRemoteWispdPath, validateSshDestination } from './wispdSshTransport.js';
import { WispdProcessTransportFactory } from './wispdTransport.js';

/** Overrides which `wispd` binary the editor runs. */
export const WISPD_PATH_ENV = 'WISP_WISPD_PATH';

/**
 * The `wispd` the editor runs, first match wins:
 *
 * 1. `WISP_WISPD_PATH`.
 * 2. The bundled binary, `<appRoot>/bin/wispd`, next to the `wisp` launcher (#62).
 * 3. In a development build, the wisp repo's `target/debug/wispd` from `cargo build -p wispd`.
 *    The Code - OSS tree is `editor/vscode` in that repo.
 * 4. `wispd` on `PATH`.
 */
export function resolveWispdExecutable(env: NodeJS.ProcessEnv, appRoot: string, isBuilt: boolean, exists: (path: string) => boolean = existsSync): string {
	const override = env[WISPD_PATH_ENV];
	if (override) {
		return override;
	}
	const bundled = join(appRoot, 'bin', 'wispd');
	if (exists(bundled)) {
		return bundled;
	}
	if (!isBuilt) {
		const repoBuild = join(appRoot, '..', '..', 'target', 'debug', 'wispd');
		if (exists(repoBuild)) {
			return repoBuild;
		}
	}
	return 'wispd';
}

/**
 * Picks the transport `wisp.host` asks for (decision record 0007): the bundled `wispd attach`
 * for `local`, or ssh for a destination, with `wisp.remoteWispdPath` in place of the default
 * PATH-then-Homebrew search. An invalid destination or path gets a factory that reports why
 * instead of ever spawning ssh. Both settings come in as `unknown`: `IConfigurationService` hands
 * back whatever JSON is in the settings file, string or not, and this never assumes it matches
 * the schema.
 */
export function createWispdTransportFactory(
	host: unknown,
	remoteWispdPath: unknown,
	localExecutable: string,
	logger: ILogger,
): IWispdTransportFactory {
	const trimmedHost = (typeof host === 'string' ? host : WISP_HOST_LOCAL).trim();
	if (trimmedHost.length === 0 || trimmedHost === WISP_HOST_LOCAL) {
		return new WispdProcessTransportFactory({ executable: localExecutable, args: ['attach'] }, logger);
	}
	const invalidHost = validateSshDestination(trimmedHost);
	if (invalidHost) {
		return new WispdInvalidHostTransportFactory(
			`ssh -- ${trimmedHost} wispd attach`,
			`wisp.host ("${trimmedHost}") is not a valid ssh destination: ${invalidHost}.`,
		);
	}
	const trimmedPath = (typeof remoteWispdPath === 'string' ? remoteWispdPath : '').trim();
	if (trimmedPath.length === 0) {
		return new WispdSshTransportFactory({ destination: trimmedHost, remoteWispdCandidates: DEFAULT_REMOTE_WISPD_CANDIDATES }, logger);
	}
	const invalidPath = validateRemoteWispdPath(trimmedPath);
	if (invalidPath) {
		return new WispdInvalidHostTransportFactory(
			`ssh -- ${trimmedHost} ${trimmedPath} attach`,
			`wisp.remoteWispdPath ("${trimmedPath}") is not an absolute path of plain characters: ${invalidPath}.`,
		);
	}
	return new WispdSshTransportFactory({ destination: trimmedHost, remoteWispdCandidates: [trimmedPath] }, logger);
}

/** The shared process's connection to wispd, served to windows over the `wispd` channel. */
export class WispdService extends Disposable implements IWispdService {
	declare readonly _serviceBrand: undefined;

	private readonly client: WispdClient;

	readonly onDidChangeState: Event<WispdState>;

	constructor(
		@INativeEnvironmentService environmentService: INativeEnvironmentService,
		@IProductService productService: IProductService,
		@ILoggerService loggerService: ILoggerService,
		@INativeHostService nativeHostService: INativeHostService,
		@ISharedProcessLifecycleService sharedProcessLifecycleService: ISharedProcessLifecycleService,
		@IConfigurationService configurationService: IConfigurationService,
	) {
		super();
		const logger = this._register(loggerService.createLogger(joinPath(environmentService.logsHome, 'wispd.log'), { id: 'wispd', name: 'wispd' }));
		const executable = resolveWispdExecutable(process.env, environmentService.appRoot, environmentService.isBuilt);
		const factory = createWispdTransportFactory(
			configurationService.getValue(WISP_HOST_SETTING),
			configurationService.getValue(WISP_REMOTE_WISPD_PATH_SETTING),
			executable,
			logger,
		);
		this.client = this._register(new WispdClient(factory, { client: { name: 'wisp', version: productService.version } }, logger));
		this.onDidChangeState = this.client.onDidChangeState;

		this._register(nativeHostService.onDidResumeOS(() => this.client.onWake()));
		this._register(sharedProcessLifecycleService.onWillShutdown(() => this.dispose()));
	}

	async getState(): Promise<WispdState> {
		this.client.start();
		return this.client.state;
	}

	async retry(): Promise<void> {
		this.client.retry();
	}

	request<M extends WispdMethod>(method: M, params: WispRequests[M]['params'], token?: CancellationToken): Promise<WispRequests[M]['result']> {
		return this.client.request(method, params, token);
	}

	subscribe(options: IWispdSubscribeOptions): Event<WispdSubscriptionMessage> {
		let subscription: IDisposable | undefined;
		const emitter = new Emitter<WispdSubscriptionMessage>({
			onWillAddFirstListener: () => {
				subscription = this.client.subscribe(options, message => emitter.fire(message));
			},
			onDidRemoveLastListener: () => {
				subscription?.dispose();
				subscription = undefined;
			},
		});
		return emitter.event;
	}
}
