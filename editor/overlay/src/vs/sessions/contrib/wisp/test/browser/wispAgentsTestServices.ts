/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { Emitter } from '../../../../../base/common/event.js';
import { DisposableStore, IDisposable, toDisposable } from '../../../../../base/common/lifecycle.js';
import { observableValue } from '../../../../../base/common/observable.js';
import { URI } from '../../../../../base/common/uri.js';
import { ICommandService } from '../../../../../platform/commands/common/commands.js';
import { IConfigurationService } from '../../../../../platform/configuration/common/configuration.js';
import { TestConfigurationService } from '../../../../../platform/configuration/test/common/testConfigurationService.js';
import { ContextKeyService } from '../../../../../platform/contextkey/browser/contextKeyService.js';
import { IContextKeyService } from '../../../../../platform/contextkey/common/contextkey.js';
import { TestInstantiationService } from '../../../../../platform/instantiation/test/common/instantiationServiceMock.js';
import { IWispdService } from '../../../../../platform/wisp/common/wispd.js';
import { WISP_HOST_SETTING } from '../../../../../platform/wisp/common/wispdConfiguration.js';
import { IViewDescriptorService } from '../../../../../workbench/common/views.js';
import { IWorkbenchEnvironmentService } from '../../../../../workbench/services/environment/common/environmentService.js';
import { ViewDescriptorService } from '../../../../../workbench/services/views/browser/viewDescriptorService.js';
import { TestEnvironmentService, TestPathService, workbenchInstantiationService } from '../../../../../workbench/test/browser/workbenchTestServices.js';
import { ISessionsProvidersChangeEvent, ISessionsProvidersService } from '../../../../services/sessions/browser/sessionsProvidersService.js';
import { ISessionsService } from '../../../../services/sessions/browser/sessionsService.js';
import { ISession } from '../../../../services/sessions/common/session.js';
import { ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';
import { ISessionsChangeEvent, ISessionsManagementService } from '../../../../services/sessions/common/sessionsManagement.js';
import { IWispAgentsService, WispAgentsService } from '../../../providers/wisp/browser/wispAgentsService.js';
import { WispSessionsProvider } from '../../../providers/wisp/browser/wispSessionsProvider.js';
import { IWispProjectsService, WispProjectsService } from '../../../providers/wisp/browser/wispProjectsService.js';
import { IWispThreadsService, WispThreadsService } from '../../../providers/wisp/browser/wispThreadsService.js';
import { IWispHostStatusService, WispHostStatusService } from '../../browser/wispHostStatusService.js';
import { TestWispdService } from './wispHostTestUtils.js';

export class TestSessionsProvidersService implements ISessionsProvidersService {
	declare readonly _serviceBrand: undefined;
	private readonly providers = new Map<string, ISessionsProvider>();
	private readonly emitter = new Emitter<ISessionsProvidersChangeEvent>();
	readonly onDidChangeProviders = this.emitter.event;

	registerProvider(provider: ISessionsProvider): IDisposable {
		this.providers.set(provider.id, provider);
		return toDisposable(() => this.providers.delete(provider.id));
	}

	getProviders(): ISessionsProvider[] {
		return [...this.providers.values()];
	}

	getProvider<T extends ISessionsProvider>(providerId: string): T | undefined {
		return this.providers.get(providerId) as T | undefined;
	}

	dispose(): void {
		this.emitter.dispose();
	}
}

/** The two methods of `ISessionsManagementService` wisp's views read, over one provider. */
class TestSessionsManagementService {
	private readonly emitter = new Emitter<ISessionsChangeEvent>();
	readonly onDidChangeSessions = this.emitter.event;
	private readonly listener: IDisposable;

	constructor(private readonly provider: WispSessionsProvider) {
		this.listener = provider.onDidChangeSessions(event => this.emitter.fire(event));
	}

	getSessions(): ISession[] {
		return this.provider.getSessions();
	}

	archiveSession(session: ISession): Promise<void> {
		return this.provider.archiveSession(session.sessionId);
	}

	deleteSession(session: ISession): Promise<void> {
		return this.provider.deleteSession(session.sessionId);
	}

	dispose(): void {
		this.listener.dispose();
		this.emitter.dispose();
	}
}

export interface IAgentsWindowServices {
	readonly instantiationService: TestInstantiationService;
	readonly viewDescriptorService: ViewDescriptorService;
	readonly commands: Array<string | [string, unknown]>;
	readonly wispd: TestWispdService;
	readonly configuration: TestConfigurationService;
	readonly hostStatus: WispHostStatusService;
	readonly projects: WispProjectsService;
	readonly agents: WispAgentsService;
	readonly threads: WispThreadsService;
	readonly provider: WispSessionsProvider;
	/** The session `ISessionsService.activeSession` reports. */
	readonly active: ReturnType<typeof observableValue<ISession | undefined>>;
	/** Resources passed to `ISessionsService.openSession`. */
	readonly opened: URI[];
}

function environment(isSessionsWindow: boolean): IWorkbenchEnvironmentService {
	return Object.create(TestEnvironmentService, { isSessionsWindow: { value: isSessionsWindow } });
}

/**
 * Upstream's workbench test services, with a test wispd and wisp's own services on top: the host
 * status, the projects, the agent runs, the normal threads, and the sessions provider, which `ISessionsManagementService` reads.
 */
export function agentsWindowServices(disposables: Pick<DisposableStore, 'add'>, isSessionsWindow: boolean, host = 'local'): IAgentsWindowServices {
	const configuration = new TestConfigurationService({ [WISP_HOST_SETTING]: host });
	const instantiationService = workbenchInstantiationService({
		environmentService: () => environment(isSessionsWindow),
		pathService: () => new TestPathService(URI.file('/Users/ryan')),
		configurationService: () => configuration,
	}, disposables as DisposableStore);
	instantiationService.stub(IContextKeyService, disposables.add(instantiationService.createInstance(ContextKeyService)));
	const viewDescriptorService = disposables.add(instantiationService.createInstance(ViewDescriptorService));
	instantiationService.stub(IViewDescriptorService, viewDescriptorService);
	const commands: Array<string | [string, unknown]> = [];
	instantiationService.stub(ICommandService, { executeCommand: async (id: string, arg?: unknown) => { commands.push(arg === undefined ? id : [id, arg]); return undefined; } });
	const wispd = disposables.add(new TestWispdService());
	instantiationService.stub(IWispdService, wispd);
	instantiationService.stub(IConfigurationService, configuration);
	const hostStatus = disposables.add(instantiationService.createInstance(WispHostStatusService));
	instantiationService.stub(IWispHostStatusService, hostStatus);
	const projects = disposables.add(instantiationService.createInstance(WispProjectsService));
	instantiationService.stub(IWispProjectsService, projects);
	const agents = disposables.add(instantiationService.createInstance(WispAgentsService));
	instantiationService.stub(IWispAgentsService, agents);
	const threads = disposables.add(instantiationService.createInstance(WispThreadsService));
	instantiationService.stub(IWispThreadsService, threads);
	const provider = disposables.add(instantiationService.createInstance(WispSessionsProvider));
	instantiationService.stub(ISessionsManagementService, disposables.add(new TestSessionsManagementService(provider)) as unknown as ISessionsManagementService);
	const active = observableValue<ISession | undefined>('active', undefined);
	const opened: URI[] = [];
	instantiationService.stub(ISessionsService, { activeSession: active, openSession: async (resource: URI) => { opened.push(resource); } } as unknown as ISessionsService);
	instantiationService.stub(ISessionsProvidersService, disposables.add(new TestSessionsProvidersService()));
	return { instantiationService, viewDescriptorService, commands, wispd, configuration, hostStatus, projects, agents, threads, provider, active, opened };
}

/** Lets promises that are already settled run their callbacks. */
export async function settle(): Promise<void> {
	for (let i = 0; i < 10; i++) {
		await Promise.resolve();
	}
}
