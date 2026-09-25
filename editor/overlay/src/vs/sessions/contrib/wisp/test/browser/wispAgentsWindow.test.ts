/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { mainWindow } from '../../../../../base/browser/window.js';
import { Emitter } from '../../../../../base/common/event.js';
import { IDisposable, toDisposable } from '../../../../../base/common/lifecycle.js';
import { URI } from '../../../../../base/common/uri.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { ICommandService } from '../../../../../platform/commands/common/commands.js';
import { ContextKeyService } from '../../../../../platform/contextkey/browser/contextKeyService.js';
import { IContextKeyService } from '../../../../../platform/contextkey/common/contextkey.js';
import { TestInstantiationService } from '../../../../../platform/instantiation/test/common/instantiationServiceMock.js';
import { Registry } from '../../../../../platform/registry/common/platform.js';
import { IViewContainersRegistry, IViewDescriptorService, IViewsRegistry, ViewContainerLocation, Extensions as ViewExtensions, WindowEnablement } from '../../../../../workbench/common/views.js';
import { IWorkbenchEnvironmentService } from '../../../../../workbench/services/environment/common/environmentService.js';
import { ViewDescriptorService } from '../../../../../workbench/services/views/browser/viewDescriptorService.js';
import { TestEnvironmentService, TestPathService, workbenchInstantiationService } from '../../../../../workbench/test/browser/workbenchTestServices.js';
import { CustomViewService, ICustomViewService } from '../../../../services/customView/browser/customViewService.js';
import { ISessionsProvidersService, ISessionsProvidersChangeEvent } from '../../../../services/sessions/browser/sessionsProvidersService.js';
import { ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';
import { WispSessionsProviderContribution } from '../../../providers/wisp/browser/wispSessionsProvider.contribution.js';
import { WISP_SESSIONS_PROVIDER_ID, WispSessionsProvider } from '../../../providers/wisp/browser/wispSessionsProvider.js';
import { setARIAContainer } from '../../../../../base/browser/ui/aria/aria.js';
import { IConfigurationService } from '../../../../../platform/configuration/common/configuration.js';
import { TestConfigurationService } from '../../../../../platform/configuration/test/common/testConfigurationService.js';
import { IWispdService } from '../../../../../platform/wisp/common/wispd.js';
import { WISP_HOST_SETTING } from '../../../../../platform/wisp/common/wispdConfiguration.js';
import { hostMenuItems, WISP_RETRY_COMMAND, WISP_SHOW_HOST_MENU_COMMAND, WISP_SHOW_LOG_COMMAND, WISP_SWITCH_HOST_COMMAND } from '../../browser/wispHostMenu.js';
import { IWispHostStatusService, WispHostStatusService } from '../../browser/wispHostStatusService.js';
import { WispNoHostContribution } from '../../browser/wispNoHost.contribution.js';
import { WISP_NO_HOST_VIEW_ID, WispNoHostView } from '../../browser/wispNoHostView.js';
import '../../browser/wispThreads.contribution.js';
import { connected, connecting, disconnected, SSH_COMMAND, TestWispdService } from './wispHostTestUtils.js';
import { WISP_THREADS_CONTAINER_ID, WISP_THREADS_VIEW_ID, WispThreadsView } from '../../browser/wispThreadsView.js';

const NO_HOST_BODY = 'The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.';

function environment(isSessionsWindow: boolean): IWorkbenchEnvironmentService {
	return Object.create(TestEnvironmentService, { isSessionsWindow: { value: isSessionsWindow } });
}

class TestSessionsProvidersService implements ISessionsProvidersService {
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

suite('wisp: Agents window', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	interface IServices {
		instantiationService: TestInstantiationService;
		viewDescriptorService: ViewDescriptorService;
		commands: Array<string | [string, unknown]>;
		wispd: TestWispdService;
		configuration: TestConfigurationService;
		hostStatus: WispHostStatusService;
	}

	function services(isSessionsWindow: boolean, host = 'local'): IServices {
		const configuration = new TestConfigurationService({ [WISP_HOST_SETTING]: host });
		const instantiationService = workbenchInstantiationService({
			environmentService: () => environment(isSessionsWindow),
			pathService: () => new TestPathService(URI.file('/Users/ryan')),
			configurationService: () => configuration,
		}, disposables);
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
		return { instantiationService, viewDescriptorService, commands, wispd, configuration, hostStatus };
	}

	/** Lets the service's first `getState` answer arrive. */
	async function settle(): Promise<void> {
		for (let i = 0; i < 5; i++) {
			await Promise.resolve();
		}
	}

	function query<T extends Element>(root: Element, selector: string): T {
		const element = root.querySelector<T>(selector);
		assert.ok(element, `${selector} is not rendered`);
		return element;
	}

	suite('sidebar', () => {

		test('wisp.threads is the sidebar\'s default container in the Agents window and holds the view', () => {
			const { viewDescriptorService } = services(true);
			const container = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry).get(WISP_THREADS_CONTAINER_ID);
			assert.ok(container);
			assert.strictEqual(container.windowEnablement, WindowEnablement.Sessions);
			assert.strictEqual(viewDescriptorService.getViewContainerLocation(container), ViewContainerLocation.Sidebar);
			assert.strictEqual(viewDescriptorService.getDefaultViewContainer(ViewContainerLocation.Sidebar)?.id, WISP_THREADS_CONTAINER_ID);
			assert.deepStrictEqual(viewDescriptorService.getViewContainerModel(container).activeViewDescriptors.map(view => view.id), [WISP_THREADS_VIEW_ID]);

			const view = Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).getView(WISP_THREADS_VIEW_ID);
			assert.strictEqual(view?.windowEnablement, WindowEnablement.Sessions);
			assert.strictEqual(view?.canToggleVisibility, false);
		});

		test('the editor window does not get it', () => {
			const { viewDescriptorService } = services(false);
			assert.strictEqual(viewDescriptorService.getViewContainerById(WISP_THREADS_CONTAINER_ID), null);
			assert.notStrictEqual(viewDescriptorService.getDefaultViewContainer(ViewContainerLocation.Sidebar)?.id, WISP_THREADS_CONTAINER_ID);
		});

		function renderSidebar(host?: string): IServices & { view: WispThreadsView } {
			const context = services(true, host);
			const view = disposables.add(context.instantiationService.createInstance(WispThreadsView, { id: WISP_THREADS_VIEW_ID, title: 'Wisp' }));
			view.render();
			return { ...context, view };
		}

		test('shows the actions, with New Chat and Customize disabled and no Automations', () => {
			const { view } = renderSidebar();
			const actions = query<HTMLElement>(view.element, '.wisp-threads-actions');
			assert.strictEqual(actions.getAttribute('role'), 'group');
			const buttons = [...actions.querySelectorAll<HTMLButtonElement>('button.wisp-threads-action')];
			assert.deepStrictEqual(buttons.map(button => [button.textContent, button.getAttribute('aria-disabled')]), [
				['New Chat', 'true'],
				['Search', null],
				['Customize', 'true'],
			]);
			assert.strictEqual(buttons[0].disabled, false, 'a disabled button would leave the tab order');
			assert.strictEqual(buttons[0].getAttribute('aria-description'), 'Connect to a host to start a chat.');
		});

		test('shows Projects with its empty copy, and hides Repositories and No Repo', () => {
			const { view } = renderSidebar();
			const sections = [...view.element.querySelectorAll<HTMLElement>('section.wisp-threads-section')];
			assert.deepStrictEqual(sections.map(section => section.querySelector('h2')?.textContent), ['Projects']);
			const projects = sections[0];
			assert.strictEqual(projects.getAttribute('aria-labelledby'), projects.querySelector('h2')?.id);
			assert.strictEqual(query(projects, '.wisp-threads-empty').textContent, 'No projects yet. A project runs on a host, so it shows here once wisp connects to one.');
			const newProject = query<HTMLButtonElement>(projects, 'button');
			assert.strictEqual(newProject.getAttribute('aria-label'), 'New Project');
			assert.strictEqual(newProject.getAttribute('aria-disabled'), 'true');
		});

		test('shows the user, the host chip, and a settings gear', async () => {
			const { view, commands } = renderSidebar();
			await Promise.resolve();
			assert.strictEqual(query(view.element, '.wisp-threads-user-name').textContent, 'ryan');
			assert.strictEqual(query(view.element, '.wisp-threads-avatar').textContent, 'R');

			const host = query<HTMLButtonElement>(view.element, 'button.wisp-threads-host');
			assert.strictEqual(host.getAttribute('aria-label'), 'Host: this Mac, not connected');
			assert.strictEqual(host.textContent, 'Not connected');
			assert.strictEqual(host.dataset.mark, 'idle');
			assert.ok(host.querySelector('.wisp-threads-host-dot'));

			const settings = query<HTMLButtonElement>(view.element, 'button.wisp-threads-settings');
			assert.strictEqual(settings.getAttribute('aria-label'), 'Settings');
			settings.click();
			assert.deepStrictEqual(commands, ['workbench.action.openSettings']);
		});

		test('the host chip follows the connection, and clicking it opens the host menu', () => {
			const { view, wispd, commands } = renderSidebar('mac-mini');
			const host = query<HTMLButtonElement>(view.element, 'button.wisp-threads-host');
			const shown = () => [host.dataset.mark, host.getAttribute('aria-label'), query(host, '.wisp-threads-host-name').textContent, query(host, '.wisp-threads-host-state').textContent];

			wispd.setState(connecting(1, SSH_COMMAND));
			assert.deepStrictEqual(shown(), ['connecting', 'Host: mac-mini, connecting', 'mac-mini', 'connecting']);

			wispd.setState(connected(SSH_COMMAND));
			assert.deepStrictEqual(shown(), ['connected', 'Host: mac-mini, connected', 'mac-mini', '']);

			wispd.setState(disconnected('exited', SSH_COMMAND));
			assert.deepStrictEqual(shown(), ['error', 'Host: mac-mini, offline', 'mac-mini', 'offline']);

			host.click();
			assert.deepStrictEqual(commands, [WISP_SHOW_HOST_MENU_COMMAND]);
		});

		test('a host state change is announced politely, but not the first render or connecting', () => {
			const aria = mainWindow.document.createElement('div');
			setARIAContainer(aria);
			const { wispd } = renderSidebar('mac-mini');
			const regions = [...aria.querySelectorAll<HTMLElement>('.monaco-status')];
			const announced = () => regions.map(element => element.textContent).join('');
			assert.strictEqual(announced(), '');

			wispd.setState(connecting(1, SSH_COMMAND));
			assert.strictEqual(announced(), '', 'connecting is transient, so it is not announced');

			wispd.setState(connected(SSH_COMMAND));
			assert.strictEqual(announced(), 'Host: mac-mini, connected');
			assert.ok(regions.every(element => element.getAttribute('aria-live') === 'polite'));
		});
	});

	suite('provider', () => {

		test('registers as wisp with no sessions, session types, or workspaces', () => {
			const { instantiationService } = services(true);
			const providers = disposables.add(new TestSessionsProvidersService());
			instantiationService.stub(ISessionsProvidersService, providers);
			disposables.add(instantiationService.createInstance(WispSessionsProviderContribution));

			const provider = providers.getProvider<WispSessionsProvider>(WISP_SESSIONS_PROVIDER_ID);
			assert.ok(provider instanceof WispSessionsProvider);
			assert.deepStrictEqual(provider.getSessions(), []);
			assert.deepStrictEqual(provider.sessionTypes, []);
			assert.deepStrictEqual(provider.getSessionTypes(URI.file('/repo')), []);
			assert.deepStrictEqual(provider.browseActions, []);
			assert.strictEqual(provider.resolveWorkspace(URI.file('/repo')), undefined);
			assert.strictEqual(provider.supportsLocalWorkspaces, false);
			assert.strictEqual(provider.supportsQuickChats, false);
			assert.deepStrictEqual(provider.getModelsSnapshot('any').models, []);
		});

		test('refuses to create or send, since no host is connected', async () => {
			const provider = disposables.add(new WispSessionsProvider());
			assert.throws(() => provider.createNewSession(URI.file('/repo'), 'wisp.thread'), /No host connected/);
			assert.throws(() => provider.createQuickChat('wisp.thread'), /No host connected/);
			await assert.rejects(provider.sendRequest('a', URI.file('/chat'), { query: 'hi' }), /No host connected/);
		});
	});

	suite('host status service', () => {

		test('takes the first state from getState, then follows events', async () => {
			const { wispd, hostStatus } = services(true);
			await settle();
			assert.strictEqual(hostStatus.status.get().kind, 'notConnected');
			wispd.setState(connecting(1));
			assert.strictEqual(hostStatus.status.get().kind, 'connecting');
			wispd.setState(connected());
			assert.strictEqual(hostStatus.status.get().kind, 'connected');
		});

		test('a slow first answer does not overwrite a newer event', async () => {
			const { wispd, hostStatus } = services(true);
			wispd.setState(connected());
			await settle();
			assert.strictEqual(hostStatus.status.get().kind, 'connected');
		});

		test('says Lost connection only after the host was connected, and forgets that for another host', () => {
			const { wispd, hostStatus } = services(true, 'mac-mini');
			wispd.setState(disconnected('noRoute', SSH_COMMAND));
			assert.strictEqual(hostStatus.status.get().heading, 'Can\'t reach mac-mini.');
			wispd.setState(connected(SSH_COMMAND));
			wispd.setState(disconnected('noRoute', SSH_COMMAND));
			assert.strictEqual(hostStatus.status.get().heading, 'Lost connection to mac-mini.');
			wispd.setState(disconnected('noRoute', 'ssh -- mac-studio wispd attach'));
			assert.strictEqual(hostStatus.status.get().heading, 'Can\'t reach mac-mini.', 'a new command is a new host');
		});

		test('retry goes to the wispd service', async () => {
			const { wispd, hostStatus } = services(true);
			await hostStatus.retry();
			assert.strictEqual(wispd.retries, 1);
		});
	});

	suite('host menu', () => {

		const run = { retry: () => { }, useThisMac: () => { }, addHost: () => { }, showLog: () => { } };

		function rows(items: ReturnType<typeof hostMenuItems>): Array<string | undefined> {
			return items.map(item => item.type === 'separator' ? `-- ${item.label ?? ''}` : item.id);
		}

		test('for this Mac: the current host, Add Host, Reconnect, and the log', () => {
			const { wispd, hostStatus } = services(true);
			wispd.setState(connected());
			const items = hostMenuItems(hostStatus.status.get(), 'local', run);
			assert.deepStrictEqual(rows(items), ['-- Hosts', 'current', 'add', '-- ', 'reconnect', 'log']);
			const current = items[1];
			assert.ok(current.type !== 'separator');
			assert.deepStrictEqual([current.label, current.description, current.detail, current.ariaLabel], ['This Mac', 'Connected', 'wispd 0.1.0 on this Mac', 'Host: this Mac, connected']);
		});

		test('for a remote host: This Mac is offered too, and a problem shows as the detail', () => {
			const { wispd, hostStatus } = services(true, 'mac-mini');
			wispd.setState(disconnected('hostKeyChanged', SSH_COMMAND));
			const items = hostMenuItems(hostStatus.status.get(), 'mac-mini', run);
			assert.deepStrictEqual(rows(items), ['-- Hosts', 'current', 'local', 'add', '-- ', 'reconnect', 'log']);
			const current = items[1];
			assert.ok(current.type !== 'separator');
			assert.deepStrictEqual([current.label, current.description, current.detail], ['mac-mini', 'Host key changed', 'mac-mini\'s host key has changed.']);
		});
	});

	suite('no-host view', () => {

		function renderNoHost(host?: string): IServices & { view: WispNoHostView; container: HTMLElement } {
			const context = services(true, host);
			const view = disposables.add(context.instantiationService.createInstance(WispNoHostView));
			const container = mainWindow.document.createElement('div');
			view.render(container);
			return { ...context, view, container };
		}

		test('covers the session surface until a host connects, and again when it drops', () => {
			const { instantiationService, wispd } = services(true);
			const customViewService = disposables.add(instantiationService.createInstance(CustomViewService));
			instantiationService.stub(ICustomViewService, customViewService);
			disposables.add(instantiationService.createInstance(WispNoHostContribution));
			assert.strictEqual(customViewService.activeCustomView.get()?.id, WISP_NO_HOST_VIEW_ID);

			wispd.setState(connected());
			assert.strictEqual(customViewService.activeCustomView.get(), undefined);

			wispd.setState(disconnected('exited'));
			assert.strictEqual(customViewService.activeCustomView.get()?.id, WISP_NO_HOST_VIEW_ID);
		});

		test('shows the copy for the state, and the actions for it', () => {
			const { container, wispd, commands } = renderNoHost('mac-mini');
			const root = query<HTMLElement>(container, 'div.wisp-agents-no-host');
			const actions = () => [...root.querySelectorAll<HTMLElement>('.wisp-no-host-action')].map(button => [button.dataset.action, button.textContent]);

			wispd.setState(connected(SSH_COMMAND));
			wispd.setState(disconnected('noRoute', SSH_COMMAND));
			assert.strictEqual(root.getAttribute('aria-label'), 'Lost connection to mac-mini.');
			assert.strictEqual(query(root, 'h2').textContent, 'Lost connection to mac-mini.');
			assert.strictEqual(query(root, 'p').textContent, 'wisp retries every 10 seconds. Agents already running on the host keep going.');
			assert.strictEqual(query<HTMLTextAreaElement>(root, 'textarea').placeholder, 'Reconnect to send messages');
			assert.deepStrictEqual(actions(), [['retry', 'Retry now'], ['switchHost', 'Switch host...'], ['showLog', 'Show log']]);

			const retry = query<HTMLElement>(root, '.wisp-no-host-action[data-action="retry"]');
			retry.click();
			wispd.setState(connecting(2, SSH_COMMAND));
			assert.strictEqual(query(root, 'h2').textContent, 'Lost connection to mac-mini.', 'the copy holds while retrying');
			assert.strictEqual(query<HTMLElement>(root, '.wisp-no-host-action[data-action="retry"]'), retry, 'the button, and its focus, survive');
			assert.strictEqual(retry.textContent, 'Retrying...');

			query<HTMLElement>(root, '.wisp-no-host-action[data-action="switchHost"]').click();
			query<HTMLElement>(root, '.wisp-no-host-action[data-action="showLog"]').click();
			assert.deepStrictEqual(commands, [WISP_RETRY_COMMAND, WISP_SWITCH_HOST_COMMAND, WISP_SHOW_LOG_COMMAND]);
		});

		test('the composer footer and label name the configured host', () => {
			const { container, configuration } = renderNoHost('mac-mini');
			assert.strictEqual(query(container, '.wisp-no-host-host-name').textContent, 'mac-mini');
			assert.strictEqual(query(container, 'textarea').getAttribute('aria-label'), 'Message the coordinator on mac-mini');

			configuration.setUserConfiguration(WISP_HOST_SETTING, 'local');
			configuration.onDidChangeConfigurationEmitter.fire({ affectedKeys: new Set([WISP_HOST_SETTING]), affectsConfiguration: (key: string) => key === WISP_HOST_SETTING } as never);
			assert.strictEqual(query(container, '.wisp-no-host-host-name').textContent, 'this Mac');
		});


		test('before a host connects, renders the M0 heading and body as a labeled region, with no actions', () => {
			const { container } = renderNoHost();
			const root = query<HTMLElement>(container, 'div.wisp-agents-no-host');
			assert.strictEqual(root.getAttribute('role'), 'region');
			assert.strictEqual(root.getAttribute('aria-label'), 'No host connected');
			assert.strictEqual(query(root, 'h2').textContent, 'No host connected');
			assert.strictEqual(query(root, 'p').textContent, NO_HOST_BODY);
			assert.strictEqual(query<HTMLElement>(root, '.wisp-no-host-actions').hidden, true);
		});

		test('the composer is read-only and aria-disabled, says why, and Send is disabled', () => {
			const { container } = renderNoHost();
			const input = query<HTMLTextAreaElement>(container, 'textarea.wisp-no-host-input');
			assert.strictEqual(input.readOnly, true);
			assert.strictEqual(input.disabled, false, 'a disabled textarea would leave the tab order');
			assert.strictEqual(input.getAttribute('aria-disabled'), 'true');
			assert.strictEqual(input.placeholder, 'Not connected to a host');
			const description = (input.getAttribute('aria-describedby') ?? '').split(' ').map(id => container.querySelector(`[id="${id}"]`)?.textContent);
			assert.deepStrictEqual(description, ['No host connected', NO_HOST_BODY]);

			const send = query<HTMLElement>(container, '.wisp-no-host-send');
			assert.strictEqual(send.getAttribute('role'), 'button');
			assert.strictEqual(send.getAttribute('aria-label'), 'Send');
			assert.strictEqual(send.getAttribute('aria-disabled'), 'true');
		});

		test('focusing the view focuses the composer', () => {
			const { view, container } = renderNoHost();
			mainWindow.document.body.appendChild(container);
			try {
				view.focus();
				assert.strictEqual(mainWindow.document.activeElement, query(container, 'textarea.wisp-no-host-input'));
			} finally {
				container.remove();
			}
		});
	});
});
