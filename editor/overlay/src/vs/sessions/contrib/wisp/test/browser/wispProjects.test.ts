/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { Emitter } from '../../../../../base/common/event.js';
import { IDisposable, toDisposable } from '../../../../../base/common/lifecycle.js';
import Severity from '../../../../../base/common/severity.js';
import { URI } from '../../../../../base/common/uri.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import { MenuId, MenuRegistry } from '../../../../../platform/actions/common/actions.js';
import { IFileDialogService } from '../../../../../platform/dialogs/common/dialogs.js';
import { INotificationService } from '../../../../../platform/notification/common/notification.js';
import { IQuickInputService } from '../../../../../platform/quickinput/common/quickInput.js';
import { Registry } from '../../../../../platform/registry/common/platform.js';
import { WispdError } from '../../../../../platform/wisp/common/wispd.js';
import type { ProjectCreateParams } from '../../../../../platform/wisp/common/wispProtocol.js';
import { IViewContainersRegistry, IViewsRegistry, ViewContainerLocation, Extensions as ViewExtensions, WindowEnablement } from '../../../../../workbench/common/views.js';
import { IChatSessionContentProvider, IChatSessionsExtensionPoint, IChatSessionsService } from '../../../../../workbench/contrib/chat/common/chatSessionsService.js';
import { ChatInteractivity, SessionStatus } from '../../../../services/sessions/common/session.js';
import { ISessionsProvider } from '../../../../services/sessions/common/sessionsProvider.js';
import { WispCoordinatorChat } from '../../../providers/wisp/browser/wispCoordinatorChat.js';
import { WISP_PROJECT_CAPABILITIES } from '../../../providers/wisp/browser/wispProjectSession.js';
import { WISP_SESSIONS_PROVIDER_ID } from '../../../providers/wisp/browser/wispSessionsProvider.js';
import { compactAge, projectGlyph, projectIdOf, projectResource, tildify, WISP_PROJECT_SESSION_TYPE } from '../../../providers/wisp/common/wispProjects.js';
import { WISP_COMPOSER_BRANCH_ACTION, WISP_COMPOSER_HOST_ACTION } from '../../browser/wispComposerFooter.js';
import { WispNewProjectFlow } from '../../browser/wispNewProject.js';
import { WISP_PROJECT_CONTAINER_ID, WISP_PROJECT_VIEW_ID, projectFacts } from '../../browser/wispProjectView.js';
import '../../browser/wispProject.contribution.js';
import { searchPicks } from '../../browser/wispSearch.js';
import { WISP_THREADS_VIEW_ID, WispThreadsView } from '../../browser/wispThreadsView.js';
import { agentsWindowServices, IAgentsWindowServices, settle } from './wispAgentsTestServices.js';
import { connected, connecting, disconnected, project, SSH_COMMAND } from './wispHostTestUtils.js';

const ONE = '0192f0c4-0000-7000-8000-000000000001';
const TWO = '0192f0c4-0000-7000-8000-000000000002';

suite('wisp: projects', () => {

	const disposables = ensureNoDisposablesAreLeakedInTestSuite();

	/** Services whose wispd lists `listed` at `seq` 5 and creates projects as asked. */
	function services(host = 'local', listed = [project(ONE, 'billing')]): IAgentsWindowServices & { lists: () => number } {
		const context = agentsWindowServices(disposables, true, host);
		let lists = 0;
		context.wispd.handler = async (method, params) => {
			switch (method) {
				case 'project/list':
					lists++;
					return { projects: listed, seq: 5 };
				case 'project/create': {
					const { id, name, repoPath } = params as ProjectCreateParams;
					return { project: project(id, name, { repoPath }) };
				}
			}
			throw new Error(`unexpected ${method}`);
		};
		return { ...context, lists: () => lists };
	}

	suite('catalog', () => {

		test('lists once connected, then subscribes from that seq with that log', async () => {
			const { wispd, projects, lists } = services();
			await settle();
			assert.strictEqual(projects.state.get().kind, 'idle');
			assert.strictEqual(lists(), 0, 'nothing is asked before the host connects');

			wispd.setState(connected());
			await settle();
			assert.strictEqual(projects.state.get().kind, 'ready');
			assert.deepStrictEqual(projects.projects.get().map(p => p.name), ['billing']);
			assert.deepStrictEqual(wispd.subscriptions.map(s => s.options), [{ after: 5, logId: 'log-1' }]);
		});

		test('project.created adds a project once, and unknown kinds are skipped', async () => {
			const { wispd, projects } = services();
			wispd.setState(connected());
			await settle();
			const event = (seq: number) => ({ type: 'event' as const, event: { seq, time: '2026-09-25T00:00:00Z', event: { kind: 'project.created' as const, project: project(TWO, 'magic') } } });
			wispd.emit(event(6));
			wispd.emit(event(6));
			wispd.emit({ type: 'event', event: { seq: 7, time: '2026-09-25T00:00:00Z', event: { kind: 'plan.proposed' } as never } });
			assert.deepStrictEqual(projects.projects.get().map(p => p.name), ['billing', 'magic']);
		});

		test('a reconnect keeps the subscription, which replays; a resync lists again', async () => {
			const { wispd, projects, lists } = services();
			wispd.setState(connected());
			await settle();
			wispd.setState(disconnected('exited'));
			wispd.setState(connecting(2));
			wispd.setState(connected());
			await settle();
			assert.strictEqual(lists(), 1, 'the connection replays what the subscription missed');
			assert.strictEqual(wispd.subscriptions.filter(s => s.active).length, 1);

			wispd.emit({ type: 'resync', reason: 'logIdChanged' });
			await settle();
			assert.strictEqual(lists(), 2);
			assert.strictEqual(projects.state.get().kind, 'ready');
			assert.strictEqual(wispd.subscriptions.filter(s => s.active).length, 1, 'the old subscription ended');
		});

		test('another host starts over, and a failed list can be retried', async () => {
			const { wispd, projects, lists } = services();
			wispd.setState(connected());
			await settle();
			assert.strictEqual(projects.projects.get().length, 1);

			const handler = wispd.handler;
			wispd.handler = async () => { throw new WispdError(-32603, 'the project store is unavailable', undefined); };
			wispd.setState(connected(SSH_COMMAND));
			await settle();
			assert.deepStrictEqual(projects.projects.get(), [], 'another host\'s projects are not this one\'s');
			const state = projects.state.get();
			assert.strictEqual(state.kind, 'failed');
			assert.match(state.kind === 'failed' ? state.message : '', /store is unavailable/);

			wispd.handler = handler;
			projects.reload();
			await settle();
			assert.strictEqual(projects.state.get().kind, 'ready');
			assert.strictEqual(lists(), 2);
		});

		test('create adds the project without waiting for its event', async () => {
			const { wispd, projects } = services();
			wispd.setState(connected());
			await settle();
			const created = await projects.create({ id: TWO, name: 'magic', repoPath: '/Users/ryan/src/magic' });
			assert.strictEqual(created.id, TWO);
			assert.deepStrictEqual(projects.projects.get().map(p => p.id), [ONE, TWO]);
		});
	});

	suite('provider', () => {

		test('publishes one wisp.project session per project, with the coordinator as its only chat', async () => {
			const { wispd, provider } = services();
			const events: string[] = [];
			disposables.add(provider.onDidChangeSessions(e => events.push(`+${e.added.length} ~${e.changed.length} -${e.removed.length}`)));
			wispd.setState(connected());
			await settle();

			const [session] = provider.getSessions();
			assert.strictEqual(session.sessionType, WISP_PROJECT_SESSION_TYPE);
			assert.strictEqual(session.providerId, WISP_SESSIONS_PROVIDER_ID);
			assert.strictEqual(session.resource.toString(), projectResource(ONE).toString());
			assert.strictEqual(session.title.get(), 'billing');
			assert.strictEqual(session.status.get(), SessionStatus.Completed);
			const chats = session.chats.get();
			assert.strictEqual(chats.length, 1);
			assert.strictEqual(chats[0], session.mainChat.get());
			assert.strictEqual(chats[0].resource.toString(), session.resource.toString());
			assert.strictEqual(chats[0].interactivity.get(), ChatInteractivity.Full);
			assert.deepStrictEqual(chats[0].capabilities?.get(), { canRename: false, canDelete: false });
			assert.deepStrictEqual(events, ['+1 ~0 -0']);

			wispd.emit({ type: 'event', event: { seq: 6, time: '2026-09-25T00:00:00Z', event: { kind: 'project.created', project: project(TWO, 'magic') } } });
			assert.deepStrictEqual(provider.getSessions().map(s => s.title.get()), ['billing', 'magic']);
			assert.deepStrictEqual(events, ['+1 ~0 -0', '+1 ~0 -0']);
		});

		test('capabilities are truthful: one chat, no forks, side chats, rename, delete, models, or new sessions', async () => {
			const { wispd, provider } = services();
			wispd.setState(connected());
			await settle();
			assert.deepStrictEqual(provider.getSessions()[0].capabilities.get(), WISP_PROJECT_CAPABILITIES);
			assert.deepStrictEqual(WISP_PROJECT_CAPABILITIES, { supportsMultipleChats: false, supportsFork: false, supportsSideChat: false, supportsRename: false, supportsDelete: false, supportsRemoveArtifacts: false });
			assert.deepStrictEqual(provider.sessionTypes, []);
			assert.strictEqual(provider.supportsQuickChats, false);
			assert.strictEqual((provider as ISessionsProvider).automations, undefined);
			assert.deepStrictEqual(provider.getModelsSnapshot('any').models, []);
			assert.strictEqual(provider.getModelPickerOptions('any').showAutoModel, false);
			await assert.rejects(provider.forkChat('a', URI.file('/c'), 't'));
			await assert.rejects(provider.createSideChat('a', URI.file('/c'), 't'));
			await assert.rejects(provider.renameSession('a', 'x'));
			await assert.rejects(provider.deleteSession('a'));
		});

		test('sessions report the connection as their remote status', async () => {
			const { wispd, provider } = services('mac-mini');
			wispd.setState(connected(SSH_COMMAND));
			await settle();
			const status = provider.getSessions()[0].remoteConnectionStatus!;
			assert.deepStrictEqual(status.get(), { kind: 'connected' });
			wispd.setState(disconnected('exited', SSH_COMMAND));
			assert.deepStrictEqual(status.get(), { kind: 'reconnecting', nextAttemptAt: 10_000 });
			wispd.setState(connecting(2, SSH_COMMAND));
			assert.deepStrictEqual(status.get(), { kind: 'reconnecting' });
		});

		test('a project on this Mac has its repository as its workspace', async () => {
			const local = services();
			local.wispd.setState(connected());
			await settle();
			const workspace = local.provider.getSessions()[0].workspace.get();
			assert.strictEqual(workspace?.folders[0].workingDirectory.fsPath, URI.file('/Users/ryan/src/billing').fsPath);
			assert.strictEqual(workspace?.label, 'billing');
		});

		test('a project on another host has no workspace, since v1 can\'t open a host\'s folders', async () => {
			const remote = services('mac-mini');
			remote.wispd.setState(connected(SSH_COMMAND));
			await settle();
			assert.strictEqual(remote.provider.getSessions()[0].workspace.get(), undefined);
		});
	});

	suite('coordinator thread', () => {

		test('registers its chat type and content provider in process, with sending off', async () => {
			const { instantiationService, wispd, projects } = services();
			const contributions: IChatSessionsExtensionPoint[] = [];
			const providers = new Map<string, IChatSessionContentProvider>();
			instantiationService.stub(IChatSessionsService, {
				registerChatSessionContribution: (contribution: IChatSessionsExtensionPoint): IDisposable => { contributions.push(contribution); return toDisposable(() => contributions.splice(contributions.indexOf(contribution), 1)); },
				registerChatSessionContentProvider: (scheme: string, provider: IChatSessionContentProvider): IDisposable => { providers.set(scheme, provider); return toDisposable(() => providers.delete(scheme)); },
			} as unknown as IChatSessionsService);
			const coordinator = instantiationService.createInstance(WispCoordinatorChat);

			assert.deepStrictEqual(contributions.map(c => c.type), [WISP_PROJECT_SESSION_TYPE]);
			const contribution = contributions[0];
			assert.strictEqual(contribution.welcomeTitle, 'Start with a goal');
			assert.match(contribution.welcomeMessage ?? '', /runs agents in their own worktrees on this Mac/);
			assert.strictEqual(contribution.requiresCustomModels, true, 'no models until accounts (M2), so the send precondition fails');
			assert.strictEqual(contribution.supportsAutoModel, false);
			assert.strictEqual(contribution.canDelegate, false);
			assert.strictEqual(contribution.requiresCopilotSignIn, false);
			assert.strictEqual(contribution.inputPlaceholder, 'The coordinator can\'t take messages yet');

			wispd.setState(connected());
			await settle();
			assert.strictEqual(projects.projects.get().length, 1);
			const content = await providers.get(WISP_PROJECT_SESSION_TYPE)!.provideChatSessionContent(projectResource(ONE), { isCancellationRequested: false } as never);
			assert.deepStrictEqual(content.history, []);
			assert.strictEqual(content.title, 'billing');
			assert.strictEqual(content.requestHandler, undefined);
			content.dispose();

			coordinator.dispose();
			assert.deepStrictEqual(contributions, []);
			assert.strictEqual(providers.size, 0);
		});
	});

	suite('sidebar', () => {

		function renderSidebar(host?: string) {
			const context = services(host, [project(ONE, 'billing', { updatedAt: new Date(Date.now() - 2 * 60_000).toISOString() }), project(TWO, 'magic-link', { updatedAt: new Date(Date.now() - 26 * 3_600_000).toISOString() })]);
			const view = disposables.add(context.instantiationService.createInstance(WispThreadsView, { id: WISP_THREADS_VIEW_ID, title: 'Wisp' }));
			view.render();
			return { ...context, view };
		}

		const rows = (view: WispThreadsView) => [...view.element.querySelectorAll<HTMLButtonElement>('button.wisp-threads-row')];

		test('lists projects under Projects: glyph, name, and relative age, newest first', async () => {
			const { view, wispd } = renderSidebar();
			wispd.setState(connected());
			await settle();
			assert.deepStrictEqual(rows(view).map(row => [
				row.querySelector('.wisp-threads-glyph')?.textContent,
				row.querySelector('.wisp-threads-row-title')?.textContent,
				row.querySelector('.wisp-threads-row-age')?.textContent,
			]), [['B', 'billing', '2m'], ['M', 'magic-link', '1d']]);
			assert.match(rows(view)[0].getAttribute('aria-label') ?? '', /^billing, project, updated /);
			assert.strictEqual(view.element.querySelector<HTMLElement>('.wisp-threads-empty')?.hidden, true);
		});

		test('a row opens its project through ISessionsService, and the active one is selected', async () => {
			const { view, wispd, opened, active, provider } = renderSidebar();
			wispd.setState(connected());
			await settle();
			rows(view)[1].click();
			assert.deepStrictEqual(opened.map(uri => uri.toString()), [projectResource(TWO).toString()]);

			active.set(provider.getSessions().find(s => s.title.get() === 'magic-link'), undefined);
			assert.deepStrictEqual(rows(view).map(row => row.getAttribute('aria-current')), [null, 'true']);
		});

		test('+ and the empty state\'s New Project run the new-project command once a host connects', async () => {
			const context = services('local', []);
			const view = disposables.add(context.instantiationService.createInstance(WispThreadsView, { id: WISP_THREADS_VIEW_ID, title: 'Wisp' }));
			view.render();
			const plus = view.element.querySelector<HTMLButtonElement>('button.wisp-threads-new-project')!;
			assert.strictEqual(plus.getAttribute('aria-disabled'), 'true');
			plus.click();
			assert.deepStrictEqual(context.commands, []);

			context.wispd.setState(connected());
			await settle();
			assert.strictEqual(plus.getAttribute('aria-disabled'), null);
			plus.click();
			const empty = view.element.querySelector<HTMLElement>('.wisp-threads-empty')!;
			assert.match(empty.textContent ?? '', /No projects yet. A project is a repository on this Mac/);
			empty.querySelector<HTMLButtonElement>('button')!.click();
			assert.deepStrictEqual(context.commands, ['wisp.newProject', 'wisp.newProject']);
		});
	});

	suite('new project', () => {

		/** An input box double that accepts the answers given, in order, or hides when they run out. */
		function quickInput(answers: string[], shown: Array<{ value: string; validationMessage: string | undefined; severity: Severity }>) {
			return {
				createInputBox: () => {
					const accept = new Emitter<void>();
					const hide = new Emitter<void>();
					const change = new Emitter<string>();
					const box = {
						value: '', title: '', prompt: '', placeholder: '', ignoreFocusOut: false, validationMessage: undefined as string | undefined, severity: Severity.Ignore,
						onDidAccept: accept.event, onDidHide: hide.event, onDidChangeValue: change.event,
						show: () => {
							shown.push({ value: box.value, validationMessage: box.validationMessage, severity: box.severity });
							const answer = answers.shift();
							queueMicrotask(() => {
								if (answer === undefined) {
									hide.fire();
								} else {
									box.value = answer;
									change.fire(answer);
									accept.fire();
								}
							});
						},
						hide: () => hide.fire(),
						dispose: () => { accept.dispose(); hide.dispose(); change.dispose(); },
					};
					return box;
				},
			} as unknown as IQuickInputService;
		}

		test('on this Mac: folder picker, then name, then an idempotent project/create, then it opens', async () => {
			const { instantiationService, wispd, opened } = services();
			wispd.setState(connected());
			await settle();
			const shown: Array<{ value: string; validationMessage: string | undefined; severity: Severity }> = [];
			instantiationService.stub(IFileDialogService, { showOpenDialog: async () => [URI.file('/Users/ryan/src/billing-service')] } as unknown as IFileDialogService);
			instantiationService.stub(IQuickInputService, quickInput(['Billing'], shown));
			const created = await instantiationService.createInstance(WispNewProjectFlow).run();

			assert.strictEqual(shown[0].value, 'billing-service', 'the name defaults to the folder\'s');
			const creates = wispd.requests.filter(([method]) => method === 'project/create').map(([, params]) => params as ProjectCreateParams);
			assert.strictEqual(creates.length, 1);
			assert.strictEqual(creates[0].name, 'Billing');
			assert.strictEqual(creates[0].repoPath, URI.file('/Users/ryan/src/billing-service').fsPath);
			assert.match(creates[0].id, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/);
			assert.strictEqual(created?.id, creates[0].id);
			assert.deepStrictEqual(opened.map(uri => projectIdOf(uri)), [creates[0].id]);
		});

		test('on another host: a typed path, and wispd\'s refusal reopens it with the reason, keeping the id', async () => {
			const { instantiationService, wispd } = services('mac-mini');
			wispd.setState(connected(SSH_COMMAND));
			await settle();
			const handler = wispd.handler!;
			wispd.handler = async (method, params) => {
				if (method === 'project/create' && (params as ProjectCreateParams).repoPath === '/Users/ryan/notes') {
					throw new WispdError(-32000, '/Users/ryan/notes is not the top folder of a git repository: it has no .git.', 'notARepository');
				}
				return handler(method, params);
			};
			const shown: Array<{ value: string; validationMessage: string | undefined; severity: Severity }> = [];
			instantiationService.stub(IQuickInputService, quickInput(['/Users/ryan/notes', 'notes', '/Users/ryan/src/notes'], shown));
			const created = await instantiationService.createInstance(WispNewProjectFlow).run();

			assert.deepStrictEqual(shown.map(s => [s.value, s.validationMessage]), [
				['', undefined],
				['notes', undefined],
				['/Users/ryan/notes', '/Users/ryan/notes is not the top folder of a git repository: it has no .git.'],
			]);
			assert.strictEqual(shown[2].severity, Severity.Error);
			const creates = wispd.requests.filter(([method]) => method === 'project/create').map(([, params]) => params as ProjectCreateParams);
			assert.deepStrictEqual(creates.map(c => c.repoPath), ['/Users/ryan/notes', '/Users/ryan/src/notes']);
			assert.strictEqual(creates[0].id, creates[1].id, 'one id per run');
			assert.strictEqual(created?.repoPath, '/Users/ryan/src/notes');
		});

		test('without a host, it says so and asks nothing', async () => {
			const { instantiationService, wispd } = services();
			const infos: string[] = [];
			instantiationService.stub(INotificationService, { info: (message: string) => infos.push(message) } as unknown as INotificationService);
			assert.strictEqual(await instantiationService.createInstance(WispNewProjectFlow).run(), undefined);
			assert.deepStrictEqual(infos, ['Connect to a host to start a project.']);
			assert.deepStrictEqual(wispd.requests, []);
		});
	});

	suite('search, Project tab, and composer footer', () => {

		test('search picks every project, newest first, and names each one', async () => {
			const { wispd, provider } = services('local', [project(ONE, 'billing'), project(TWO, 'magic', { updatedAt: '2026-09-25T00:00:00Z' })]);
			wispd.setState(connected());
			await settle();
			const sessions = [...provider.getSessions()].sort((a, b) => b.updatedAt.get().getTime() - a.updatedAt.get().getTime());
			const picks = searchPicks(sessions, Date.parse('2026-09-25T00:03:00Z'));
			assert.deepStrictEqual(picks.map(pick => [pick.label, pick.description, pick.ariaLabel]), [['magic', '3m', 'magic, project'], ['billing', '12h', 'billing, project']]);
			assert.deepStrictEqual(picks.map(pick => projectIdOf(pick.resource)), [TWO, ONE]);
		});

		test('the Project tab is the auxiliary bar\'s first default container, in the Agents window only', () => {
			const containers = Registry.as<IViewContainersRegistry>(ViewExtensions.ViewContainersRegistry);
			const container = containers.get(WISP_PROJECT_CONTAINER_ID);
			assert.ok(container);
			assert.strictEqual(container.windowEnablement, WindowEnablement.Sessions);
			assert.strictEqual(container.order, 0);
			assert.strictEqual(containers.getViewContainerLocation(container), ViewContainerLocation.AuxiliaryBar);
			assert.strictEqual(containers.getDefaultViewContainers(ViewContainerLocation.AuxiliaryBar)[0]?.id, WISP_PROJECT_CONTAINER_ID);
			const view = Registry.as<IViewsRegistry>(ViewExtensions.ViewsRegistry).getView(WISP_PROJECT_VIEW_ID);
			assert.strictEqual(view?.windowEnablement, WindowEnablement.Sessions);
			assert.strictEqual(view?.canToggleVisibility, false);
		});

		test('the Project tab lists the repository from wispd, and says what isn\'t there yet', () => {
			assert.deepStrictEqual(projectFacts(project(ONE, 'billing'), 'this Mac', '/Users/ryan').map(fact => [fact.label, fact.detail, fact.done]), [
				['Repository', '~/src/billing on this Mac, branch main', true],
				['Plan', 'None yet', false],
				['Shared context', '0 files', false],
				['Coordinator account', 'Not set', false],
			]);
			assert.strictEqual(projectFacts(project(ONE, 'billing', { branch: undefined, repoPath: '/srv/billing' }), 'mac-mini', undefined)[0].detail, '/srv/billing on mac-mini');
		});

		test('the composer footer shows the branch and host in wisp.project threads only', () => {
			const items = MenuRegistry.getMenuItems(MenuId.ChatInputSecondary).filter(item => 'command' in item && (item.command.id === WISP_COMPOSER_BRANCH_ACTION || item.command.id === WISP_COMPOSER_HOST_ACTION));
			assert.deepStrictEqual(items.map(item => 'command' in item ? item.command.id : ''), [WISP_COMPOSER_BRANCH_ACTION, WISP_COMPOSER_HOST_ACTION]);
			for (const item of items) {
				assert.strictEqual(item.when?.serialize(), `chatSessionType == '${WISP_PROJECT_SESSION_TYPE}'`);
			}
		});

		test('helpers: glyphs are stable letters, ages are compact, and home is ~', () => {
			assert.deepStrictEqual(projectGlyph({ id: ONE, name: '  billing' }), projectGlyph({ id: ONE, name: 'billing' }));
			assert.strictEqual(projectGlyph({ id: ONE, name: '42 things' }).letter, '4');
			assert.strictEqual(projectGlyph({ id: ONE, name: '---' }).letter, '#');
			const now = Date.parse('2026-09-25T12:00:00Z');
			assert.deepStrictEqual(['2026-09-25T11:59:30Z', '2026-09-25T11:58:00Z', '2026-09-25T07:00:00Z', '2026-09-22T12:00:00Z', '2026-09-10T12:00:00Z', '2026-06-25T12:00:00Z', '2024-09-25T12:00:00Z'].map(date => compactAge(new Date(date), now)), ['now', '2m', '5h', '3d', '2w', '3mo', '2y']);
			assert.strictEqual(tildify('/Users/ryan/src/wisp', '/Users/ryan'), '~/src/wisp');
			assert.strictEqual(tildify('/Users/ryanx/src', '/Users/ryan'), '/Users/ryanx/src');
			assert.strictEqual(projectIdOf(projectResource(ONE)), ONE);
			assert.strictEqual(projectIdOf(URI.file('/x')), undefined);
		});
	});
});
