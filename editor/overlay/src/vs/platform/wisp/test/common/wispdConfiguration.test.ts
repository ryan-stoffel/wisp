/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import assert from 'assert';
import { ensureNoDisposablesAreLeakedInTestSuite } from '../../../../base/test/common/utils.js';
import { ConfigurationModelParser } from '../../../configuration/common/configurationModels.js';
import { ConfigurationScope, Extensions as ConfigurationExtensions, IConfigurationRegistry } from '../../../configuration/common/configurationRegistry.js';
import { NullLogService } from '../../../log/common/log.js';
import { Registry } from '../../../registry/common/platform.js';
import { WISP_HOST_LOCAL, WISP_HOST_SETTING, WISP_REMOTE_WISPD_PATH_SETTING } from '../../common/wispdConfiguration.js';

/** Everything a folder's `.vscode/settings.json` is parsed with; excludes `APPLICATION`. */
const WORKSPACE_SCOPES = [ConfigurationScope.WINDOW, ConfigurationScope.RESOURCE, ConfigurationScope.LANGUAGE_OVERRIDABLE, ConfigurationScope.MACHINE_OVERRIDABLE];

/** Everything a user's own `settings.json` is parsed with. */
const APPLICATION_AND_WORKSPACE_SCOPES = [ConfigurationScope.APPLICATION, ConfigurationScope.MACHINE, ConfigurationScope.APPLICATION_MACHINE, ...WORKSPACE_SCOPES];

suite('wispdConfiguration', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('wisp.host and wisp.remoteWispdPath are registered as application scope', () => {
		const properties = Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).getConfigurationProperties();
		assert.strictEqual(properties[WISP_HOST_SETTING]?.scope, ConfigurationScope.APPLICATION);
		assert.strictEqual(properties[WISP_HOST_SETTING]?.default, WISP_HOST_LOCAL);
		assert.strictEqual(properties[WISP_REMOTE_WISPD_PATH_SETTING]?.scope, ConfigurationScope.APPLICATION);
		assert.strictEqual(properties[WISP_REMOTE_WISPD_PATH_SETTING]?.default, '');
	});

	test('a workspace settings.json cannot set wisp.host', () => {
		const parser = new ConfigurationModelParser('.vscode/settings.json', new NullLogService());
		parser.parse(JSON.stringify({ [WISP_HOST_SETTING]: 'attacker.example' }), { scopes: WORKSPACE_SCOPES });
		assert.strictEqual(parser.configurationModel.getValue(WISP_HOST_SETTING), undefined);
	});

	test('a user settings.json can set wisp.host', () => {
		const parser = new ConfigurationModelParser('settings.json', new NullLogService());
		parser.parse(JSON.stringify({ [WISP_HOST_SETTING]: 'mac-mini.local' }), { scopes: APPLICATION_AND_WORKSPACE_SCOPES });
		assert.strictEqual(parser.configurationModel.getValue(WISP_HOST_SETTING), 'mac-mini.local');
	});
});
