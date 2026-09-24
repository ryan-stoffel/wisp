import { app, BrowserWindow } from 'electron';
import { PAGE_PATH, windowOptions } from './window.ts';

async function openWindow(): Promise<void> {
  const mainWindow = new BrowserWindow(windowOptions());
  await mainWindow.loadFile(PAGE_PATH);
}

app.on('window-all-closed', () => {
  app.quit();
});

// Not top-level await: Electron starts only after the entry module finishes evaluating, so it would never be ready.
app
  .whenReady()
  .then(openWindow)
  .catch((error: unknown) => {
    console.error(error);
    app.exit(1);
  });
