/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { DisposableStore } from '../../../../base/common/lifecycle.js';
import { basename } from '../../../../base/common/path.js';
import Severity from '../../../../base/common/severity.js';
import { localize, localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { IFileDialogService } from '../../../../platform/dialogs/common/dialogs.js';
import { IInstantiationService, ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { INotificationService } from '../../../../platform/notification/common/notification.js';
import { IQuickInputService } from '../../../../platform/quickinput/common/quickInput.js';
import { generateUuidV7 } from '../../../../platform/wisp/common/uuidv7.js';
import { WispdError } from '../../../../platform/wisp/common/wispd.js';
import { isLocalHost } from '../../../../platform/wisp/common/wispdConfiguration.js';
import type { Project } from '../../../../platform/wisp/common/wispProtocol.js';
import { ISessionsService } from '../../../services/sessions/browser/sessionsService.js';
import { IWispProjectsService } from '../../providers/wisp/browser/wispProjectsService.js';
import { projectResource } from '../../providers/wisp/common/wispProjects.js';
import { IWispHostStatusService } from './wispHostStatusService.js';

export const WISP_NEW_PROJECT_COMMAND = 'wisp.newProject';

/** wispd's limits on `project/create` (decision record 0009). */
const MAX_NAME_BYTES = 256;
const MAX_PATH_BYTES = 1024;

const encoder = new TextEncoder();

interface IAskOptions {
	readonly title: string;
	readonly prompt: string;
	readonly placeholder?: string;
	readonly value: string;
	/** Shown until the text changes, such as wispd's reason for refusing a path. */
	readonly problem?: string;
	readonly validate: (text: string) => string | undefined;
}

/**
 * Creates a project: the repository folder on the host, then a name, then `project/create`.
 *
 * On this Mac the folder comes from the native folder picker. On another host it is typed as a
 * path, since v1 can't browse a host's folders (#67). Either way wispd decides whether the folder
 * is a repository, and when it says no, the path step opens again with its reason.
 *
 * The id is made once per run, so a request resent after a reconnect never makes a second
 * project (decision record 0007).
 */
export class WispNewProjectFlow {

	constructor(
		@IWispHostStatusService private readonly hostStatusService: IWispHostStatusService,
		@IWispProjectsService private readonly projectsService: IWispProjectsService,
		@IFileDialogService private readonly fileDialogService: IFileDialogService,
		@IQuickInputService private readonly quickInputService: IQuickInputService,
		@INotificationService private readonly notificationService: INotificationService,
		@ISessionsService private readonly sessionsService: ISessionsService,
	) { }

	async run(): Promise<Project | undefined> {
		const status = this.hostStatusService.status.get();
		if (status.kind !== 'connected') {
			this.notificationService.info(localize('wispNewProject.noHost', "Connect to a host to start a project."));
			return undefined;
		}
		const host = status.host;
		const local = isLocalHost(this.hostStatusService.configuredHost.get());
		const id = generateUuidV7();

		let path = local ? await this.pickFolder() : await this.askPath(host, '', undefined);
		if (path === undefined) {
			return undefined;
		}
		const name = await this.askName(basename(path));
		if (name === undefined) {
			return undefined;
		}
		while (true) {
			let project: Project;
			try {
				project = await this.projectsService.create({ id, name, repoPath: path });
			} catch (error) {
				if (error instanceof WispdError && error.kind === 'notARepository') {
					path = await this.askPath(host, path, error.message);
					if (path === undefined) {
						return undefined;
					}
					continue;
				}
				this.notificationService.error(localize('wispNewProject.failed', "Couldn't create the project: {0}", error instanceof Error ? error.message : String(error)));
				return undefined;
			}
			await this.sessionsService.openSession(projectResource(project.id));
			return project;
		}
	}

	private async pickFolder(): Promise<string | undefined> {
		const picked = await this.fileDialogService.showOpenDialog({
			title: localize('wispNewProject.pickTitle', "New Project: Choose the Repository"),
			openLabel: localize('wispNewProject.pickLabel', "Choose"),
			canSelectFolders: true,
			canSelectFiles: false,
			canSelectMany: false,
		});
		return picked?.[0]?.fsPath;
	}

	private askPath(host: string, value: string, problem: string | undefined): Promise<string | undefined> {
		return this.ask({
			title: localize('wispNewProject.pathTitle', "New Project on {0}", host),
			prompt: localize('wispNewProject.pathPrompt', "The repository's folder on {0}, as an absolute path", host),
			placeholder: '/Users/you/src/app',
			value,
			problem,
			validate: text => {
				if (!text.startsWith('/')) {
					return localize('wispNewProject.pathAbsolute', "Enter an absolute path, starting with /.");
				}
				if (text.includes('\0') || encoder.encode(text).length > MAX_PATH_BYTES) {
					return localize('wispNewProject.pathLength', "Enter a path of at most {0} bytes.", MAX_PATH_BYTES);
				}
				return undefined;
			},
		});
	}

	private askName(value: string): Promise<string | undefined> {
		return this.ask({
			title: localize('wispNewProject.nameTitle', "New Project: Name"),
			prompt: localize('wispNewProject.namePrompt', "The project's name, as the sidebar shows it"),
			value,
			validate: text => {
				if (text.length === 0) {
					return localize('wispNewProject.nameEmpty', "Enter a name.");
				}
				if (text.includes('\0') || encoder.encode(text).length > MAX_NAME_BYTES) {
					return localize('wispNewProject.nameLength', "Enter a name of at most {0} bytes.", MAX_NAME_BYTES);
				}
				return undefined;
			},
		});
	}

	/** An input box that shows `problem` until the text changes, and accepts only valid text, trimmed. */
	private ask(options: IAskOptions): Promise<string | undefined> {
		const store = new DisposableStore();
		const box = store.add(this.quickInputService.createInputBox());
		box.title = options.title;
		box.prompt = options.prompt;
		box.placeholder = options.placeholder;
		box.value = options.value;
		box.ignoreFocusOut = true;
		const setProblem = (message: string | undefined) => {
			box.validationMessage = message;
			box.severity = message ? Severity.Error : Severity.Ignore;
		};
		setProblem(options.problem);
		return new Promise<string | undefined>(resolve => {
			let accepted: string | undefined;
			store.add(box.onDidChangeValue(() => setProblem(undefined)));
			store.add(box.onDidAccept(() => {
				const text = box.value.trim();
				const problem = options.validate(text);
				if (problem) {
					setProblem(problem);
					return;
				}
				accepted = text;
				box.hide();
			}));
			store.add(box.onDidHide(() => {
				store.dispose();
				resolve(accepted);
			}));
			box.show();
		});
	}
}

registerAction2(class NewProjectAction extends Action2 {
	constructor() {
		super({
			id: WISP_NEW_PROJECT_COMMAND,
			title: localize2('wispNewProject.title', "New Project..."),
			category: localize2('wisp.category', "Wisp"),
			f1: true,
		});
	}

	async run(accessor: ServicesAccessor): Promise<void> {
		await accessor.get(IInstantiationService).createInstance(WispNewProjectFlow).run();
	}
});
