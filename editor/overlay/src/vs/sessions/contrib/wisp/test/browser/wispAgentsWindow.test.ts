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
import { WispNoHostContribution } from '../../browser/wispNoHost.contribution.js';
import { WISP_NO_HOST_VIEW_ID, WispNoHostView } from '../../browser/wispNoHostView.js';
import '../../browser/wispThreads.contribution.js';
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

	function services(isSessionsWindow: boolean): { instantiationService: TestInstantiationService; viewDescriptorService: ViewDescriptorService; commands: string[] } {
		const instantiationService = workbenchInstantiationService({
			environmentService: () => environment(isSessionsWindow),
			pathService: () => new TestPathService(URI.file('/Users/ryan')),
		}, disposables);
		instantiationService.stub(IContextKeyService, disposables.add(instantiationService.createInstance(ContextKeyService)));
		const viewDescriptorService = disposables.add(instantiationService.createInstance(ViewDescriptorService));
		instantiationService.stub(IViewDescriptorService, viewDescriptorService);
		const commands: string[] = [];
		instantiationService.stub(ICommandService, { executeCommand: async (id: string) => { commands.push(id); return undefined; } });
		return { instantiationService, viewDescriptorService, commands };
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

		function renderSidebar(): { view: WispThreadsView; commands: string[] } {
			const { instantiationService, commands } = services(true);
			const view = disposables.add(instantiationService.createInstance(WispThreadsView, { id: WISP_THREADS_VIEW_ID, title: 'Wisp' }));
			view.render();
			return { view, commands };
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

		test('shows the user, a static Not connected host chip, and a settings gear', async () => {
			const { view, commands } = renderSidebar();
			await Promise.resolve();
			assert.strictEqual(query(view.element, '.wisp-threads-user-name').textContent, 'ryan');
			assert.strictEqual(query(view.element, '.wisp-threads-avatar').textContent, 'R');

			const host = query<HTMLElement>(view.element, '.wisp-threads-host');
			assert.strictEqual(host.getAttribute('role'), 'status');
			assert.strictEqual(host.getAttribute('aria-live'), 'polite');
			assert.strictEqual(host.getAttribute('aria-label'), 'Host: not connected');
			assert.strictEqual(host.textContent, 'Not connected');
			assert.ok(host.querySelector('.wisp-threads-host-dot'));

			const settings = query<HTMLButtonElement>(view.element, 'button.wisp-threads-settings');
			assert.strictEqual(settings.getAttribute('aria-label'), 'Settings');
			settings.click();
			assert.deepStrictEqual(commands, ['workbench.action.openSettings']);
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

	suite('no-host view', () => {

		function renderNoHost(): { view: WispNoHostView; container: HTMLElement } {
			const view = disposables.add(new WispNoHostView());
			const container = mainWindow.document.createElement('div');
			view.render(container);
			return { view, container };
		}

		test('is shown over the session surface while no host is connected', () => {
			const { instantiationService } = services(true);
			const customViewService = disposables.add(instantiationService.createInstance(CustomViewService));
			instantiationService.stub(ICustomViewService, customViewService);
			disposables.add(instantiationService.createInstance(WispNoHostContribution));
			assert.strictEqual(customViewService.activeCustomView.get()?.id, WISP_NO_HOST_VIEW_ID);
		});

		test('renders the heading and body as a labeled region', () => {
			const { container } = renderNoHost();
			const root = query<HTMLElement>(container, 'div.wisp-agents-no-host');
			assert.strictEqual(root.getAttribute('role'), 'region');
			assert.strictEqual(root.getAttribute('aria-label'), 'No host connected');
			assert.strictEqual(query(root, 'h2').textContent, 'No host connected');
			assert.strictEqual(query(root, 'p').textContent, NO_HOST_BODY);
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
