import { join } from 'node:path';
import type { BrowserWindowConstructorOptions } from 'electron';

export const WINDOW_TITLE = 'wisp';

export const PAGE_PATH = join(import.meta.dirname, '..', 'static', 'index.html');

export function windowOptions(): BrowserWindowConstructorOptions {
  return {
    title: WINDOW_TITLE,
    width: 1280,
    height: 800,
    backgroundColor: '#1f1f1f',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  };
}
