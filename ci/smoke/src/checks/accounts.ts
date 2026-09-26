// Clicking Sign in on a detected CLI in the Accounts view (#121) opens an integrated terminal
// running that vendor's login command (decision record 0004, #115), over the local host's real
// wispd -- the packaged app's bundled binary, started on demand (0010) since `wisp.host` defaults
// to `local`. A fake `claude` on PATH answers wispd's own detection probe (#114) as installed and
// signed in, and answers the terminal's own "claude auth login" by writing its argv to a proof
// file. wisp itself never reads a sign-in terminal's output (0004); this check does, only to prove
// which command actually ran, the same way editor.ts's terminal check proves a real command ran.
import { existsSync, readFileSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { launch, type LaunchOptions } from '../../../screenshots/src/harness.ts';
import { check } from '../check.ts';
import { appLaunchOptions, ready } from '../harness.ts';

const fakeCliDir = join(import.meta.dirname, '..', '..', 'fixtures', 'fake-cli');

export const accountsChecks = [
  check('Sign in on a detected CLI opens a terminal running its login command', async () => {
    const options = await appLaunchOptions();
    const home = await mkdtemp(join(tmpdir(), 'wisp-smoke-home-'));
    const proofDir = await mkdtemp(join(tmpdir(), 'wisp-smoke-signin-'));
    const proofFile = join(proofDir, 'proof.txt');
    const withFakeCli: LaunchOptions = {
      ...options,
      env: {
        ...options.env,
        HOME: home,
        PATH: `${fakeCliDir}${delimiter}${process.env.PATH ?? ''}`,
        WISP_SIGNIN_PROOF_FILE: proofFile,
      },
    };
    let step = 'launch';
    const session = await launch(withFakeCli);
    try {
      const { window } = session;
      await ready(session);

      step = 'the Accounts view opens from the command palette';
      await window.keyboard.press('ControlOrMeta+Shift+KeyP');
      const paletteInput = window.locator('.quick-input-widget input');
      await paletteInput.waitFor({ state: 'visible' });
      await paletteInput.fill('>Wisp: Show Accounts');
      const showAccounts = window
        .locator('.quick-input-widget .monaco-list-row')
        .filter({ hasText: 'Show Accounts' })
        .first();
      await showAccounts.waitFor({ state: 'visible', timeout: 20_000 });
      await showAccounts.click();
      await window.locator('.wisp-accounts').waitFor({ state: 'visible' });

      step = 'wispd detects the fake claude as installed and signed in';
      const claudeRow = window
        .locator('.wisp-accounts-list .wisp-accounts-row')
        .filter({ hasText: 'Claude Code' })
        .first();
      await claudeRow.waitFor({ state: 'visible', timeout: 30_000 });

      step = 'Sign in is enabled once claude is detected';
      const signIn = claudeRow.getByRole('button', { name: 'Sign in' });
      await window.waitForFunction(
        (selector: string) => {
          const button = document.querySelector(selector);
          return button !== null && button.getAttribute('aria-disabled') !== 'true';
        },
        '.wisp-accounts-list .wisp-accounts-row .wisp-accounts-link-button',
        { timeout: 30_000 },
      );

      step = 'clicking Sign in opens a terminal';
      await signIn.click();
      const terminal = window.locator('.terminal-wrapper .xterm, .terminal .xterm').first();
      await terminal.waitFor({ state: 'visible', timeout: 20_000 });

      step = 'the terminal runs claude auth login';
      const deadline = Date.now() + 15_000;
      while (!existsSync(proofFile) && Date.now() < deadline) {
        await window.waitForTimeout(250);
      }
      if (!existsSync(proofFile)) {
        throw new Error(`the terminal never ran the fake claude (expected ${proofFile})`);
      }
      const argv = readFileSync(proofFile, 'utf8').trim();
      if (argv !== 'auth login') {
        throw new Error(`expected the terminal to run "claude auth login", got argv "${argv}"`);
      }
    } catch (error) {
      throw new Error(`${step}: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
    } finally {
      await session.close();
      await rm(home, { recursive: true, force: true, maxRetries: 3 });
      await rm(proofDir, { recursive: true, force: true, maxRetries: 3 });
    }
  }),
];
