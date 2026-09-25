// Driving New Project end to end (sessions/contrib/wisp/browser/wispNewProject.ts,
// wispThreadsView.ts's sidebar "+"). On this Mac it opens the native folder picker
// (IFileDialogService), which Playwright can't see or click, so Electron's own dialog module is
// stubbed in the main process first -- the same technique, and the same flow (the sidebar's "+",
// not the command palette), as ci/screenshots/src/scenarios.ts's own `answerFolderPicker` and
// `createProject`, which this reuses to stay consistent with what's already proven there.
import type { ElectronApplication, Page } from 'playwright-core';

interface ElectronDialog {
  showOpenDialog: () => Promise<{ canceled: boolean; filePaths: string[] }>;
}

/** Makes the next native folder picker resolve to `folderPath`, with no dialog ever shown. */
export async function stubFolderPicker(app: ElectronApplication, folderPath: string): Promise<void> {
  // Typed inline, the way ci/screenshots/src/harness.ts's fitWindow types its own BrowserWindow
  // handle: the "electron" package isn't a dependency here, so its own types aren't resolvable.
  await app.evaluate(
    (electron: { dialog: ElectronDialog }, chosen: string) => {
      electron.dialog.showOpenDialog = () => Promise.resolve({ canceled: false, filePaths: [chosen] });
    },
    folderPath,
  );
}

/**
 * Clicks the sidebar's New Project "+" for a folder already stubbed with {@link stubFolderPicker},
 * and accepts the name step's suggested default (the folder's own name).
 */
export async function createLocalProject(window: Page): Promise<void> {
  await window.locator('.part.sidebar button.wisp-threads-new-project').click();
  const name = window.locator('.quick-input-widget input');
  await name.waitFor({ state: 'visible' });
  await name.press('Enter');
}
