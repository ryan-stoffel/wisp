// A plain launch (no arguments) opens the Agents window: wisp's sidebar, the no-host view, and nothing
// upstream would show to get a user signed in to Copilot (decision 0011; ci/screenshots/src/scenarios.ts's
// agents-window scenario makes the same no-host composer check for the screenshot itself).
import { check } from '../check.ts';
import { launchSmoke, ready, visible } from '../harness.ts';
import { titleBarText, visibleChatElements } from '../ui.ts';

export const agentsWindowChecks = [
  check('plain launch opens the Agents window with no sign-in or Copilot UI', async () => {
    const session = await launchSmoke();
    try {
      const { app, window } = session;
      await ready({ app, window });

      await visible(window, '.part.titlebar', '.part.sidebar .wisp-threads', '.wisp-agents-no-host');

      const input = window.locator('.wisp-agents-no-host textarea');
      const send = window.locator('.wisp-agents-no-host .wisp-no-host-send');
      const composer = {
        readonly: await input.evaluate((element) => (element as HTMLTextAreaElement).readOnly),
        inputAriaDisabled: await input.getAttribute('aria-disabled'),
        sendAriaDisabled: await send.getAttribute('aria-disabled'),
      };
      if (!composer.readonly || composer.inputAriaDisabled !== 'true' || composer.sendAriaDisabled !== 'true') {
        throw new Error(`the no-host composer is not disabled: ${JSON.stringify(composer)}`);
      }

      const dialogs = await window.locator('.monaco-dialog-box').count();
      if (dialogs !== 0) {
        throw new Error(`expected no dialog, found ${String(dialogs)}`);
      }

      const titleBar = await titleBarText(window);
      const signInOrAccount = titleBar.filter((text) => /sign in|copilot|account/i.test(text));
      if (signInOrAccount.length > 0) {
        throw new Error(`title bar shows sign-in or Copilot text: ${signInOrAccount.join(', ')}`);
      }

      const chat = await visibleChatElements(window);
      if (chat.length > 0) {
        throw new Error(`Agents window shows Chat UI: ${chat.join(', ')}`);
      }
    } finally {
      await session.close();
    }
  }),
];
