/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import '../browser/wisp.contribution.js';
import './wispd.contribution.js';

import { KeyCode, KeyMod } from '../../../../base/common/keyCodes.js';
import { localize2 } from '../../../../nls.js';
import { Action2, registerAction2 } from '../../../../platform/actions/common/actions.js';
import { ServicesAccessor } from '../../../../platform/instantiation/common/instantiation.js';
import { KeybindingWeight } from '../../../../platform/keybinding/common/keybindingsRegistry.js';
import { INativeHostService } from '../../../../platform/native/common/native.js';

const category = localize2('wisp', "Wisp");

// Upstream's Open Agents Window is disabled while AI features are off, which they are in this
// window. The Agents window is wisp's main UI, so the editor window keeps a way back to it.
registerAction2(class extends Action2 {
	constructor() {
		super({
			id: 'wisp.openAgentsWindow',
			title: localize2('wisp.openAgentsWindow', "Open Agents Window"),
			category,
			f1: true,
			keybinding: {
				primary: KeyMod.CtrlCmd | KeyMod.Shift | KeyCode.KeyA,
				weight: KeybindingWeight.WorkbenchContrib,
			},
		});
	}

	run(accessor: ServicesAccessor): Promise<void> {
		return accessor.get(INativeHostService).openAgentsWindow();
	}
});
