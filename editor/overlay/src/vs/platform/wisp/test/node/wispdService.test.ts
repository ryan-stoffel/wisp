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

	test('local, or unset, or blank, all use the bundled wispd', () => {
		for (const host of ['local', undefined, '', '  ']) {
			assert.ok(createWispdTransportFactory(host, undefined, '/bin/wispd', logger) instanceof WispdProcessTransportFactory);
		}
	});

	test('a valid destination uses ssh, with the default candidates when no path is set', () => {
		const factory = createWispdTransportFactory('mac-mini.local', undefined, '/bin/wispd', logger);
		assert.ok(factory instanceof WispdSshTransportFactory);
		assert.match(factory.command, /^ssh .* -- mac-mini\.local wispd attach$/);
	});

	test('an invalid destination never spawns ssh', () => {
		const factory = createWispdTransportFactory('-oProxyCommand=x', undefined, '/bin/wispd', logger);
		assert.ok(factory instanceof WispdInvalidHostTransportFactory);
	});

	test('a valid absolute remoteWispdPath is the only candidate tried, and shows up in the command', () => {
		const factory = createWispdTransportFactory('mac-mini.local', '/opt/homebrew/bin/wispd', '/bin/wispd', logger);
		assert.ok(factory instanceof WispdSshTransportFactory);
		assert.match(factory.command, /-- mac-mini\.local \/opt\/homebrew\/bin\/wispd attach$/);
	});

	test('an invalid remoteWispdPath never spawns ssh, even with a valid host', () => {
		const factory = createWispdTransportFactory('mac-mini.local', '/x; touch /tmp/p', '/bin/wispd', logger);
		assert.ok(factory instanceof WispdInvalidHostTransportFactory);
	});

	test('a blank remoteWispdPath falls back to the default candidates', () => {
		const factory = createWispdTransportFactory('mac-mini.local', '   ', '/bin/wispd', logger);
		assert.ok(factory instanceof WispdSshTransportFactory);
		assert.match(factory.command, /-- mac-mini\.local wispd attach$/);
	});

	test('a non-string host is treated as unset, not as a crash', () => {
		assert.ok(createWispdTransportFactory(1 as unknown as string, undefined, '/bin/wispd', logger) instanceof WispdProcessTransportFactory);
		assert.ok(createWispdTransportFactory(true as unknown as string, undefined, '/bin/wispd', logger) instanceof WispdProcessTransportFactory);
		assert.ok(createWispdTransportFactory(null as unknown as string, undefined, '/bin/wispd', logger) instanceof WispdProcessTransportFactory);
	});

	test('a non-string remoteWispdPath is treated as unset, not as a crash', () => {
		const factory = createWispdTransportFactory('mac-mini.local', 42 as unknown as string, '/bin/wispd', logger);
		assert.ok(factory instanceof WispdSshTransportFactory);
		assert.match(factory.command, /-- mac-mini\.local wispd attach$/);
	});
});
