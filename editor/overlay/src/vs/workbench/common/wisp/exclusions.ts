/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

// Upstream workbench features that wisp leaves out. The view container, view, and workbench
// contribution registries skip these ids, so the features are never registered.

const excludedViewContainers: ReadonlySet<string> = new Set([
	// Chat: the coordinator replaces it and must be the secondary side bar's default container.
	'workbench.panel.chat',
	// Run and Debug and the Debug Console: wisp ships no debuggers.
	'workbench.view.debug',
	'workbench.panel.repl',
	// Ports: local port forwarding goes through Microsoft dev tunnels.
	'~remote.forwardedPortsContainer',
]);

const excludedWorkbenchContributions: ReadonlySet<string> = new Set([
	// Chat setup: Copilot sign-in and setup, the title bar Sign In button, and "Use AI Features with Copilot".
	'workbench.contrib.chatSetup',
	// The Copilot status bar item.
	'workbench.contrib.chatStatusBarEntry',
	// The remote indicator: its status bar slot becomes wisp's host status.
	'workbench.contrib.remoteStatusIndicator',
]);

export function isExcludedViewContainer(id: string): boolean {
	return excludedViewContainers.has(id);
}

export function isExcludedWorkbenchContribution(id: string): boolean {
	return excludedWorkbenchContributions.has(id);
}
