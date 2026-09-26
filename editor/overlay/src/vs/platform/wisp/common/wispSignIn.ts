/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

import type { CliKind } from './wispProtocol.js';
import { isLocalHost, validateSshDestination } from './wispdConfiguration.js';

/**
 * A vendor CLI's login command (decision record 0004, #115): run directly on `local`, or on a
 * remote host's shell over ssh. wisp never runs it itself and never reads what it prints; it only
 * opens the terminal that runs it.
 */
export interface SignInCommand {
	readonly executable: string;
	readonly args: readonly string[];
	readonly env?: Readonly<Record<string, string>>;
}

/**
 * 0004's login command for each CLI. Codex and Cursor differ on a remote host, since an
 * interactive OAuth flow can't reach a browser there: Codex adds `--device-auth`, and Cursor sets
 * `NO_OPEN_BROWSER=1`.
 */
export function loginCommandForCli(cli: CliKind, remote: boolean): SignInCommand {
	switch (cli) {
		case 'claude':
			return { executable: 'claude', args: ['auth', 'login'] };
		case 'codex':
			return remote
				? { executable: 'codex', args: ['login', '--device-auth'] }
				: { executable: 'codex', args: ['login'] };
		case 'cursor':
			return remote
				? { executable: 'agent', args: ['login'], env: { NO_OPEN_BROWSER: '1' } }
				: { executable: 'agent', args: ['login'] };
		default:
			// A newer wispd may report a kind this version doesn't know (wispProtocol.ts's own
			// comment on CliKind); fall back to the kind's own name as the binary, with no login args.
			return { executable: cli, args: [] };
	}
}

export type SignInLaunchResult =
	| { readonly kind: 'ok'; readonly launch: SignInCommand }
	| { readonly kind: 'error'; readonly message: string };

/**
 * Builds the command an integrated terminal should run to sign a detected CLI in (#115), on
 * `host` as it reads from `wisp.host`: `local`, or an ssh destination.
 *
 * On `local` this is just the CLI's own login command. On a remote host it is
 * `ssh -t -- <destination> <cli> <login args>`, reusing #64's own `validateSshDestination` so a
 * destination that could be misread as an ssh option, or that could inject a second shell command
 * once it reaches the host's login shell, is rejected before ssh ever runs. Every part is a
 * separate argv entry: wisp never assembles `destination` or the remote command into one string
 * for a shell to parse, on either side of the connection. An env var (Cursor's `NO_OPEN_BROWSER`)
 * becomes an `env NAME=value` prefix in that same argv, not a shell assignment, since ssh joins
 * its trailing arguments with spaces and hands them to the host's login shell as plain words.
 */
export function buildSignInLaunch(cli: CliKind, host: string): SignInLaunchResult {
	if (isLocalHost(host)) {
		return { kind: 'ok', launch: loginCommandForCli(cli, false) };
	}
	const destination = host.trim();
	const problem = validateSshDestination(destination);
	if (problem) {
		return { kind: 'error', message: `wisp.host (${destination}) isn't a usable ssh destination: ${problem}.` };
	}
	const command = loginCommandForCli(cli, true);
	const remoteArgs = command.env
		? ['env', ...Object.entries(command.env).map(([name, value]) => `${name}=${value}`), command.executable, ...command.args]
		: [command.executable, ...command.args];
	return { kind: 'ok', launch: { executable: 'ssh', args: ['-t', '--', destination, ...remoteArgs] } };
}

/** Plain letters, digits, and a handful of punctuation marks that never need quoting in a POSIX shell. */
const UNQUOTED_SAFE = /^[A-Za-z0-9_.\-/=:@]+$/;

/** One argument, quoted the way a POSIX shell would need it. Used only to render a command for display; wisp never hands this string to a shell. */
export function quotePosixArg(value: string): string {
	if (value.length > 0 && UNQUOTED_SAFE.test(value)) {
		return value;
	}
	return `'${value.replace(/'/g, `'\\''`)}'`;
}

/** {@link buildSignInLaunch}'s command as one line, for a notification or a log; never executed as a shell command. */
export function formatSignInCommand(launch: SignInCommand): string {
	return [launch.executable, ...launch.args].map(quotePosixArg).join(' ');
}
