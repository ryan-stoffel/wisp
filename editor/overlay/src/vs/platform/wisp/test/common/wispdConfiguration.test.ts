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

suite('wispdConfiguration', () => {

	ensureNoDisposablesAreLeakedInTestSuite();

	test('wisp.host and wisp.remoteWispdPath are registered as application scope', () => {
		const properties = Registry.as<IConfigurationRegistry>(ConfigurationExtensions.Configuration).getConfigurationProperties();
		assert.strictEqual(properties[WISP_HOST_SETTING]?.scope, ConfigurationScope.APPLICATION);
		assert.strictEqual(properties[WISP_HOST_SETTING]?.default, WISP_HOST_LOCAL);
		assert.strictEqual(properties[WISP_REMOTE_WISPD_PATH_SETTING]?.scope, ConfigurationScope.APPLICATION);
		assert.strictEqual(properties[WISP_REMOTE_WISPD_PATH_SETTING]?.default, '');
	});

	test('a workspace settings.json cannot set wisp.host or wisp.remoteWispdPath', () => {
		const parser = new ConfigurationModelParser('.vscode/settings.json', new NullLogService());
		parser.parse(JSON.stringify({ [WISP_HOST_SETTING]: 'attacker.example', [WISP_REMOTE_WISPD_PATH_SETTING]: '/tmp/wispd' }), { scopes: WORKSPACE_SCOPES });
		assert.strictEqual(parser.configurationModel.getValue(WISP_HOST_SETTING), undefined);
		assert.strictEqual(parser.configurationModel.getValue(WISP_REMOTE_WISPD_PATH_SETTING), undefined);
	});
});
