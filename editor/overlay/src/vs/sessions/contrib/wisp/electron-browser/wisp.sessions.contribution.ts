/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

// wisp's entry point in the Agents window, loaded by the `workbench: load wisp in the Agents
// window` patch. The browser side holds wisp's UI; the wispd connection lives in the shared
// process, and the file below registers it and its commands, as the editor window's does.

import '../browser/wisp.sessions.contribution.js';
import '../../../../workbench/contrib/wisp/electron-browser/wispd.contribution.js';
