/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { NullLogger } from '../../../log/common/log.js';
import { createWispdTransportFactory } from '../../node/wispdService.js';
import { WispdProcessTransportFactory } from '../../node/wispdTransport.js';
import { WispdInvalidHostTransportFactory, WispdSshTransportFactory } from '../../node/wispdSshTransport.js';

suite('createWispdTransportFactory', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	const logger = new NullLogger();

	test('local, unset, blank, or not a string, all use the bundled wispd', () => {
		for (const host of ['local', undefined, '', '  ', 1, true, null]) {
			assert.ok(createWispdTransportFactory(host, undefined, '/bin/wispd', logger) instanceof WispdProcessTransportFactory);
		}
	});

	test('a valid destination uses ssh, with the default candidates when no usable path is set', () => {
		for (const path of [undefined, '   ', 42]) {
			const factory = createWispdTransportFactory('mac-mini.local', path, '/bin/wispd', logger);
			assert.ok(factory instanceof WispdSshTransportFactory);
			assert.match(factory.command, /^ssh .* -- mac-mini\.local wispd attach$/);
		}
	});

	test('a valid absolute remoteWispdPath is the only candidate tried, and shows up in the command', () => {
		const factory = createWispdTransportFactory('mac-mini.local', '/opt/homebrew/bin/wispd', '/bin/wispd', logger);
		assert.ok(factory instanceof WispdSshTransportFactory);
		assert.match(factory.command, /-- mac-mini\.local \/opt\/homebrew\/bin\/wispd attach$/);
	});

	test('an invalid destination or remoteWispdPath never spawns ssh', () => {
		assert.ok(createWispdTransportFactory('-oProxyCommand=x', undefined, '/bin/wispd', logger) instanceof WispdInvalidHostTransportFactory);
		assert.ok(createWispdTransportFactory('mac-mini.local', '/x; touch /tmp/p', '/bin/wispd', logger) instanceof WispdInvalidHostTransportFactory);
	});
});
