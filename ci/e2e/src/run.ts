import { runChecks } from '../../smoke/src/check.ts';
import { handshakeChecks } from './checks/handshake.ts';
import { projectPersistenceChecks } from './checks/project-persistence.ts';
import { reconnectChecks } from './checks/reconnect.ts';
import { sshChecks } from './checks/ssh.ts';
import { versionMismatchChecks } from './checks/version-mismatch.ts';

await runChecks([
  ...handshakeChecks,
  ...projectPersistenceChecks,
  ...reconnectChecks,
  ...versionMismatchChecks,
  ...sshChecks,
]);
