// Sign in on a detected CLI in the Accounts view opens an integrated terminal running that vendor's
// login command (0004), over the bundled wispd. The fake `claude` on PATH answers wispd's detection
// as signed in, and its "auth login" writes its argv to a proof file, which proves which command ran.
import { existsSync, readFileSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fakeClaudeEnv, runWispCommand } from '../../../screenshots/src/agents.ts';
import { appLaunchOptions, launch, ready } from '../../../screenshots/src/harness.ts';
import { check } from '../check.ts';

export const accountsChecks = [
  check('Sign in on a detected CLI opens a terminal running its login command', async () => {
    const options = await appLaunchOptions();
    const home = await mkdtemp(join(tmpdir(), 'wisp-smoke-home-'));
    const proofDir = await mkdtemp(join(tmpdir(), 'wisp-smoke-signin-'));
    const proofFile = join(proofDir, 'proof.txt');
    let step = 'launch';
    const session = await launch({
      ...options,
      env: { ...options.env, HOME: home, ...fakeClaudeEnv(), WISP_SIGNIN_PROOF_FILE: proofFile },
    });
    try {
      const { window } = session;
      await ready(session);

      step = 'the Accounts view opens from the command palette';
      await runWispCommand(window, 'Show Accounts');
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

      step = 'clicking Sign in opens a terminal that runs claude auth login';
      await signIn.click();
      // The first terminal in a fresh app starts cold, so this waits longer than editor.ts does.
      const deadline = Date.now() + 40_000;
      while (!existsSync(proofFile) && Date.now() < deadline) {
        await window.waitForTimeout(250);
      }
      if (!existsSync(proofFile)) {
        const terminals = await window.locator('.terminal-wrapper .xterm, .terminal .xterm').count();
        throw new Error(`the terminal never ran the fake claude (expected ${proofFile}; ${String(terminals)} terminal(s) visible)`);
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
