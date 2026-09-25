/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { mainWindow } from '../../../../../base/browser/window.js';
import { KeyCode, KeyMod } from '../../../../../base/common/keyCodes.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../../../platform/configuration/common/configurationRegistry.js';
import { ContextKeyService } from '../../../../../platform/contextkey/browser/contextKeyService.js';
import { IContextKeyService } from '../../../../../platform/contextkey/common/contextkey.js';
import { TestInstantiationService } from '../../../../../platform/instantiation/test/common/instantiationServiceMock.js';
import { Registry } from '../../../../../platform/registry/common/platform.js';
import { IViewContainersRegistry, IViewDescriptorService, IViewsRegistry, ViewContainerLocation, Extensions as ViewExtensions } from '../../../../common/views.js';
import { ViewDescriptorService } from '../../../../services/views/browser/viewDescriptorService.js';
import { workbenchInstantiationService } from '../../../../test/browser/workbenchTestServices.js';
import '../../browser/wisp.contribution.js';
import { COORDINATOR_CHAT_VIEW_ID, COORDINATOR_VIEW_CONTAINER_ID, CoordinatorChatViewPane } from '../../browser/coordinatorChatView.js';

suite('wisp: coordinator chat', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();
	let instantiationService: TestInstantiationService;
	let viewDescriptorService: ViewDescriptorService;

	setup(() => {
		instantiationService = workbenchInstantiationService(undefined, disposables);
		instantiationService.stub(IContextKeyService, disposables.add(instantiationService.createInstance(ContextKeyService)));
		viewDescriptorService = disposables.add(instantiationService.createInstance(ViewDescriptorService));
		instantiationService.stub(IViewDescriptorService, viewDescriptorService);
	});

	function renderView(): CoordinatorChatViewPane {
		const view = disposables.add(instantiationService.createInstance(CoordinatorChatViewPane, { id: COORDINATOR_CHAT_VIEW_ID, title: 'Chat' }));
		view.render();
		return view;
	}

	function query<T extends Element>(view: CoordinatorChatViewPane, selector: string): T {
		const element = view.element.querySelector<T>(selector);
		assert.ok(element, `${selector} is not rendered`);
		return element;
	}

	test('the Coordinator container is the secondary side bar\'s default and holds the chat view', () => {
		const container = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry).get(COORDINATOR_VIEW_CONTAINER_ID);
		assert.ok(container);
		assert.strictEqual(container.title.value, 'Coordinator');
		assert.strictEqual(viewDescriptorService.getViewContainerLocation(container), ViewContainerLocation.AuxiliaryBar);
		assert.strictEqual(viewDescriptorService.getDefaultViewContainer(ViewContainerLocation.AuxiliaryBar)?.id, COORDINATOR_VIEW_CONTAINER_ID);
		assert.deepStrictEqual(viewDescriptorService.getViewContainerModel(container).activeViewDescriptors.map(view => view.id), [COORDINATOR_CHAT_VIEW_ID]);
	});

	test('the chat view descriptor matches the spec, with the focus command on Ctrl+Cmd+I', () => {
		const view = Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).getView(COORDINATOR_CHAT_VIEW_ID);
		assert.ok(view);
		assert.strictEqual(view.name.value, 'Chat');
		assert.strictEqual(view.singleViewPaneContainerTitle, 'Coordinator');
		assert.strictEqual(view.canToggleVisibility, false);
		assert.strictEqual(view.canMoveView, true);
		assert.strictEqual(view.focusCommand?.id, 'wisp.coordinatorChat.focus');
		assert.strictEqual(view.focusCommand?.keybindings?.mac?.primary, KeyMod.CtrlCmd | KeyMod.WinCtrl | KeyCode.KeyI);
	});

	test('the secondary side bar shows at startup in every window, and no welcome page opens', () => {
		const defaults = Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).getConfigurationDefaultsOverrides();
		assert.strictEqual(defaults.get('workbench.secondarySideBar.defaultVisibility')?.value, 'visible');
		assert.strictEqual(defaults.get('workbench.startupEditor')?.value, 'none');
		assert.strictEqual(defaults.get('chat.disableAIFeatures')?.value, true);
	});

	test('renders the no-host empty state as a labeled region', () => {
		const view = renderView();
		const root = query<HTMLElement>(view, '.wisp-coordinator-chat');
		assert.strictEqual(root.getAttribute('role'), 'region');
		assert.strictEqual(root.getAttribute('aria-label'), 'Coordinator chat');

		const context = query<HTMLElement>(view, '.wisp-chat-context');
		assert.strictEqual(context.getAttribute('role'), 'group');
		assert.strictEqual(context.getAttribute('aria-label'), 'Project and host');
		assert.strictEqual(query(view, '.wisp-chat-project').textContent, 'No project');
		assert.strictEqual(query(view, '.wisp-chat-host').textContent, 'Not connected');
		assert.strictEqual(query(view, '.wisp-chat-dot').getAttribute('aria-hidden'), 'true');

		assert.strictEqual(query(view, '.wisp-chat-empty h3').textContent, 'No host connected');
		assert.strictEqual(query(view, '.wisp-chat-empty p').textContent, 'The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.');
	});

	test('the composer is read-only and aria-disabled, and says why', () => {
		const view = renderView();
		const input = query<HTMLTextAreaElement>(view, 'textarea.wisp-chat-input');
		assert.strictEqual(input.readOnly, true);
		assert.strictEqual(input.disabled, false, 'a disabled textarea would leave the tab order');
		assert.strictEqual(input.getAttribute('aria-disabled'), 'true');
		assert.strictEqual(input.getAttribute('aria-label'), 'Message the coordinator');
		assert.strictEqual(input.placeholder, 'Not connected to a host');

		const description = (input.getAttribute('aria-describedby') ?? '').split(' ').map(id => view.element.querySelector(`[id="${id}"]`)?.textContent);
		assert.deepStrictEqual(description, [
			'No host connected',
			'The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.',
		]);

		const send = query<HTMLElement>(view, '.wisp-chat-send');
		assert.strictEqual(send.textContent, 'Send');
		assert.strictEqual(send.getAttribute('role'), 'button');
		assert.strictEqual(send.getAttribute('aria-disabled'), 'true');
	});

	test('focusing the view focuses the composer, even while it is disabled', () => {
		const view = renderView();
		mainWindow.document.body.appendChild(view.element);
		try {
			view.focus();
			assert.strictEqual(mainWindow.document.activeElement, query(view, 'textarea.wisp-chat-input'));
		} finally {
			view.element.remove();
		}
	});
});
