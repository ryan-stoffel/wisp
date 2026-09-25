/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

// Upstream workbench features that wisp leaves out. The view container, view, and workbench
// contribution registries skip these ids, so the features are never registered. The editor
// window and the Agents window share these registries, so one list covers both.

const excludedViewContainers: ReadonlySet<string> = new Set([
	// Chat: the coordinator replaces it and must be the secondary side bar's default container.
	'workbench.panel.chat',
	// Run and Debug and the Debug Console: wisp ships no debuggers.
	'workbench.view.debug',
	'workbench.panel.repl',
	// Ports: local port forwarding goes through Microsoft dev tunnels.
	'~remote.forwardedPortsContainer',
	// Agents window: upstream's Sessions list, with Automations, Chats, and AI Customizations.
	// wisp's own sidebar view replaces it (#12).
	'agentic.workbench.view.sessionsContainer',
]);

const excludedWorkbenchContributions: ReadonlySet<string> = new Set([
	// Chat setup: Copilot sign-in and setup, the title bar Sign In button, and "Use AI Features with Copilot".
	'workbench.contrib.chatSetup',
	// The Copilot status bar item.
	'workbench.contrib.chatStatusBarEntry',
	// The remote indicator: its status bar slot becomes wisp's host status.
	'workbench.contrib.remoteStatusIndicator',

	// Agents window: the Copilot account widget in the title bar.
	'workbench.contrib.sessionsWidget',
	// Agents window: AI Customizations, Copilot's skills, instructions, agents, hooks, tools, and plugins.
	'workbench.contrib.sessionsCustomizationsToolbar',
	'workbench.contrib.sessionsActiveHarnessSync',
	'sessions.customizationsDebugLog',
	// Agents window: Automations, until #18 decides whether wisp's triggers reuse them.
	'workbench.contrib.automationScheduler',
	'sessions.contrib.automationTools',
	'workbench.contrib.chatAutomationsEnabledContext',
	'sessions.contrib.automationsCustomView',
	// Agents window: the Copilot Chat sessions provider and its pickers. wisp registers its own provider (#12).
	'sessions.defaultSessionsProvider',
	'workbench.contrib.copilotPickerActionViewItems',
	'workbench.contrib.copilotPermissionPickerWeb',
	'workbench.contrib.chat.copilotConfigSlashSubmitHandler',
	'sessions.contrib.chat.copilotConfigSlashSubmitHandler',
	// Both windows: the local agent host, its sessions provider, and the utility process it starts,
	// which runs Copilot CLI and sends Claude through Copilot's API. wispd runs wisp's agents (0011).
	'workbench.contrib.agentHostPrewarm',
	'workbench.contrib.agentHostContribution',
	'workbench.contrib.agentHostTerminal',
	'workbench.contrib.agentHostCopilotCliSettings',
	'workbench.contrib.agentHostAllowSignedOutWhenUsable',
	'workbench.contrib.agentHostSignedOutModelsNotification',
	'workbench.contrib.agentHostSdkSetupNotification',
	'workbench.contrib.agentHostSessionListContribution',
	'workbench.chat.agentHostOpenSessionLinkOpener',
	'sessions.contrib.localAgentHostContribution',
	'sessions.contrib.localAgentHostLifecycle',
	// Agents window: remote agent hosts over SSH, Microsoft dev tunnels, WSL, Dev Containers, WebSockets,
	// and GitHub cloud sandboxes, plus sharing the local agent host through a tunnel.
	'sessions.contrib.remoteAgentHostContribution',
	'workbench.contrib.remoteAgentHostTerminal',
	'sessions.contrib.sshAgentHostContribution',
	'sessions.contrib.tunnelAgentHostContribution',
	'sessions.contrib.wslAgentHostContribution',
	'sessions.contrib.devContainerAgentHostConnector',
	'sessions.contrib.webSocketAgentHostContribution',
	'workbench.contrib.cloudSandboxAgentHost',
	'sessions.connectionDiagnostics',
	'workbench.contrib.sessionsTunnelHostTitlebar',
	// Agents window: the GitHub pull request and issue integration, which polls api.github.com.
	'sessions.contrib.githubPullRequestPolling',
	'workbench.contrib.agentFeedbackPRReviewSeeder',
	'workbench.contrib.agentFeedbackPRThreadResolver',
	// Agents window: Copilot onboarding tours.
	'sessions.contrib.onboardingTours.agentHostReadinessContext',
	'sessions.contrib.onboardingTours.newSessionTour',
	'sessions.contrib.onboardingTours.newSessionViewTour',
	'sessions.contrib.onboardingTours.newSessionViewV2Tour',
	'sessions.contrib.onboardingTours.newSessionViewV3Tour',
	// Agents window: Copilot voice mode.
	'sessions.voiceBridge',
	'sessions.voiceActiveSession',
	'sessions.voiceListening',
	'sessions.voiceNewComposer',
]);

export function isExcludedViewContainer(id: string): boolean {
	return excludedViewContainers.has(id);
}

export function isExcludedWorkbenchContribution(id: string): boolean {
	return excludedWorkbenchContributions.has(id);
}
