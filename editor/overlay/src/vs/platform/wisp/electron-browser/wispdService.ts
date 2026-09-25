/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import { registerSharedProcessRemoteService } from '../../ipc/electron-browser/services.js';
import { IWispdService, WISPD_CHANNEL_NAME, WispdChannelClient } from '../common/wispd.js';

registerSharedProcessRemoteService(IWispdService, WISPD_CHANNEL_NAME, { channelClientCtor: WispdChannelClient });
