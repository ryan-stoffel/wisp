import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readExclusionLists } from '../src/exclusions.ts';

test('reads every id from the committed exclusion lists', async () => {
  const lists = await readExclusionLists();

  assert.ok(lists.viewContainers.length >= 5, `expected at least 5 view containers, got ${String(lists.viewContainers.length)}`);
  assert.ok(
    lists.workbenchContributions.length >= 30,
    `expected at least 30 workbench contributions, got ${String(lists.workbenchContributions.length)}`,
  );
  assert.ok(lists.actions.length >= 4, `expected at least 4 actions, got ${String(lists.actions.length)}`);

  assert.ok(lists.viewContainers.includes('workbench.panel.chat'));
  assert.ok(lists.workbenchContributions.includes('workbench.contrib.chatSetup'));
  assert.ok(lists.actions.includes('sessions.action.titleBarAccountWidget'));

  const groups: readonly (readonly [string, readonly string[]])[] = [
    ['viewContainers', lists.viewContainers],
    ['workbenchContributions', lists.workbenchContributions],
    ['actions', lists.actions],
  ];
  for (const [group, ids] of groups) {
    assert.equal(new Set(ids).size, ids.length, `${group} has a duplicate id`);
    for (const id of ids) {
      assert.ok(id.length > 0, `${group} has an empty id`);
      assert.ok(!/['"]/.test(id), `${group} id ${JSON.stringify(id)} still has a quote character in it`);
    }
  }
});
