/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { mainWindow } from '../../../../../base/browser/window.js';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../../base/test/common/utils.js';
import type { DetectedCli } from '../../../../../platform/wisp/common/wispProtocol.js';
import { describeCliState, setDisabled } from '../../browser/wispAccountsEditor.js';

function cli(overrides: Partial<DetectedCli> = {}): DetectedCli {
	return { cli: 'claude', installed: true, ...overrides };
}

suite('wisp: accounts editor', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	suite('describeCliState', () => {

		test('names not installed, signed in, not signed in, and unknown sign-in state', () => {
			assert.strictEqual(describeCliState(cli({ installed: false })), 'Not installed');
			assert.strictEqual(describeCliState(cli({ signedIn: true })), 'Signed in');
			assert.strictEqual(describeCliState(cli({ signedIn: false })), 'Not signed in');
			assert.strictEqual(describeCliState(cli({ signedIn: undefined })), 'Installed; sign-in state unknown');
		});
	});

	suite('setDisabled', () => {

		test('sets aria-disabled and a description, and clears both when enabled again', () => {
			const button = mainWindow.document.createElement('button');
			setDisabled(button, true, 'Not available yet.');
			assert.strictEqual(button.getAttribute('aria-disabled'), 'true');
			assert.strictEqual(button.getAttribute('aria-description'), 'Not available yet.');
			assert.ok(button.classList.contains('disabled'));

			setDisabled(button, false);
			assert.strictEqual(button.getAttribute('aria-disabled'), null);
			assert.strictEqual(button.getAttribute('aria-description'), null);
			assert.ok(!button.classList.contains('disabled'));
		});
	});
});
