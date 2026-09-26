// Generated from crates/wisp-protocol by `cargo run -p wisp-protocol --bin generate-typescript`. Do not edit.
//
// The messages of the protocol between the editor and wispd (decision record 0007). The
// JSON-RPC 2.0 envelope around them is vs/base/common/jsonRpcProtocol.ts.

/** The newest protocol version these types describe. */
export const PROTOCOL_VERSION = 1;

/** The largest frame either side sends or accepts: 8 MiB, not counting the line ending. */
export const MAX_FRAME_BYTES = 8388608;

/** JSON-RPC error codes. A `WispError`'s `data` is an `ErrorData`. */
export const ErrorCodes = {
	ParseError: -32700,
	InvalidRequest: -32600,
	MethodNotFound: -32601,
	InvalidParams: -32602,
	InternalError: -32603,
	WispError: -32000,
	RequestCancelled: -32800,
} as const;

/** Requests, which the editor sends and wispd answers, by method. */
export type WispRequests = {
	/**
	 * `initialize`: the handshake. It must be the first request on a connection; wispd
	 * answers anything before it with `notInitialized`.
	 */
	"initialize": { params: InitializeParams, result: InitializeResult },
	/**
	 * `host/health`: uptime, store state, and running agents. The editor sends it every
	 * 30 seconds and on wake, as a heartbeat.
	 */
	"host/health": { params: HostHealthParams, result: HostHealthResult },
	/**
	 * `host/version`: wispd's release and protocol versions, operating system, and CPU
	 * architecture.
	 */
	"host/version": { params: HostVersionParams, result: HostVersionResult },
	/**
	 * `project/list`: every project, and the `seq` the list reflects.
	 */
	"project/list": { params: ProjectListParams, result: ProjectListResult },
	/**
	 * `project/create`: creates a project, idempotent on its client-generated id.
	 */
	"project/create": { params: ProjectCreateParams, result: ProjectCreateResult },
	/**
	 * `events/subscribe`: replays the events after a `seq`, then streams new ones as
	 * `events/event` notifications.
	 */
	"events/subscribe": { params: EventsSubscribeParams, result: EventsSubscribeResult },
	/**
	 * `events/unsubscribe`: ends a subscription.
	 */
	"events/unsubscribe": { params: EventsUnsubscribeParams, result: EventsUnsubscribeResult },
	/**
	 * `accounts/keys/add`: stores an API key in the Keychain, idempotent on its
	 * client-generated id. The response returns only the key's masked form.
	 */
	"accounts/keys/add": { params: AccountsKeysAddParams, result: AccountsKeysAddResult },
	/**
	 * `accounts/keys/list`: every key account, with its key masked.
	 */
	"accounts/keys/list": { params: AccountsKeysListParams, result: AccountsKeysListResult },
	/**
	 * `accounts/keys/remove`: removes a key account's key from the Keychain, and its
	 * record. Fails with `accountNotFound` if the id does not exist.
	 */
	"accounts/keys/remove": { params: AccountsKeysRemoveParams, result: AccountsKeysRemoveResult },
	/**
	 * `accounts/list`: the vendor CLIs wispd detects (#114), installed, signed in, and their
	 * plan where exposed. May answer from a short-lived cache. Gated on the `agentClis`
	 * capability.
	 */
	"accounts/list": { params: AccountsListParams, result: AccountsListResult },
	/**
	 * `accounts/refresh`: like `accounts/list`, but always probes again instead of using the
	 * cache. Gated on the `agentClis` capability.
	 */
	"accounts/refresh": { params: AccountsRefreshParams, result: AccountsRefreshResult },
	/**
	 * `usage/get`: per-account tokens and cost for today and this week (local time on this
	 * host), and the latest limit windows.
	 */
	"usage/get": { params: UsageGetParams, result: UsageGetResult },
	/**
	 * `accounts/defaults/get`: this host's default account for the coordinator role and for
	 * a worker role, absent where none is set (#119).
	 */
	"accounts/defaults/get": { params: AccountsDefaultsGetParams, result: AccountsDefaultsGetResult },
	/**
	 * `accounts/defaults/set`: sets or clears one role's default account, and returns both
	 * roles' defaults as they stand after the change.
	 */
	"accounts/defaults/set": { params: AccountsDefaultsSetParams, result: AccountsDefaultsGetResult },
	/**
	 * `context/list`: a project's shared context files (0005), each with its size, when it
	 * was last modified, and who last wrote it, if known.
	 */
	"context/list": { params: ContextListParams, result: ContextListResult },
	/**
	 * `context/read`: one shared context file's content. Fails with `contextNotFound` if it
	 * does not exist.
	 */
	"context/read": { params: ContextReadParams, result: ContextReadResult },
	/**
	 * `context/write`: writes a shared context file in full, idempotent on its
	 * client-generated id. The last write to a path wins when two race.
	 */
	"context/write": { params: ContextWriteParams, result: ContextWriteResult },
	/**
	 * `agent/start`: starts a worker in its own worktree of the project's repository, with
	 * the worker sandbox (0013) and the shared context folder, idempotent on its
	 * client-generated run id. Gated on the `agents` capability, like every `agent/*`
	 * method.
	 */
	"agent/start": { params: AgentStartParams, result: AgentRunResult },
	/**
	 * `agent/send`: a message to a run (0011): its next turn while it runs, or a resumed
	 * session once it has ended. Idempotent on the message's client-generated turn id.
	 */
	"agent/send": { params: AgentSendParams, result: AgentRunResult },
	/**
	 * `agent/cancel`: stops a running agent. Does nothing to a run that isn't running.
	 */
	"agent/cancel": { params: AgentCancelParams, result: AgentRunResult },
	/**
	 * `agent/list`: every run, or one project's, and the `seq` the list reflects.
	 */
	"agent/list": { params: AgentListParams, result: AgentListResult },
	/**
	 * `agent/events`: one run's events from wispd's log, a page at a time.
	 */
	"agent/events": { params: AgentEventsParams, result: AgentEventsResult },
	/**
	 * `agent/diff`: the files that differ between a run's base and its latest commit, each
	 * with its stats and a size-capped unified diff (#157). Gated on the `agentReview`
	 * capability, like every review method.
	 */
	"agent/diff": { params: AgentDiffParams, result: AgentDiffResult },
	/**
	 * `agent/file`: one file of a run's diff, on its base or head side, base64-encoded and
	 * size-capped, for a diff editor.
	 */
	"agent/file": { params: AgentFileParams, result: AgentFileResult },
	/**
	 * `agent/accept`: merges a run's commit into the project repository's current branch on
	 * the host, fast-forward when possible, then removes its worktree and branch. Never
	 * pushes. Idempotent on its client-generated id.
	 */
	"agent/accept": { params: AgentAcceptParams, result: AgentAcceptResult },
	/**
	 * `agent/requestChanges`: the reviewer's follow-up to a run, sent as `agent/send` sends
	 * a message. Idempotent on its client-generated turn id.
	 */
	"agent/requestChanges": { params: AgentRequestChangesParams, result: AgentRunResult },
	/**
	 * `thread/list`: every repo entry and normal thread, and the `seq` the list reflects
	 * (#110). Gated on the `threads` capability, like every `thread/*` and `repo/*` method.
	 */
	"thread/list": { params: ThreadListParams, result: ThreadListResult },
	/**
	 * `repo/add`: registers a repository on the host for normal threads, idempotent on its
	 * client-generated id and on its path.
	 */
	"repo/add": { params: RepoAddParams, result: RepoAddResult },
	/**
	 * `thread/start`: starts a normal thread's agent in a worktree of a repo entry, or in a
	 * scratch repository of its own when no repo is given. Idempotent on its
	 * client-generated run id.
	 */
	"thread/start": { params: ThreadStartParams, result: ThreadStartResult },
	/**
	 * `thread/archive`: archives a normal thread or brings it back.
	 */
	"thread/archive": { params: ThreadArchiveParams, result: ThreadArchiveResult },
	/**
	 * `thread/delete`: deletes a normal thread with its run, worktree, and stored events,
	 * stopping its CLI first if it runs.
	 */
	"thread/delete": { params: ThreadDeleteParams, result: ThreadDeleteResult },
};

/** Notifications, which get no response, by method. */
export type WispNotifications = {
	/**
	 * `$/cancelRequest`: cancels a request, which still gets exactly one response. Either
	 * side may send it.
	 */
	"$/cancelRequest": CancelRequestParams,
	/**
	 * `events/event`: one event for a subscription. wispd sends it.
	 */
	"events/event": EventsEventParams,
};

/**
 * Params of `initialize`, the first request on every connection.
 *
 * `protocol` and `capabilities` never change shape. To answer `incompatibleProtocol` even to a
 * client whose other params changed, decode `InitializeProtocol` before these.
 */
export type InitializeParams = {
	/**
	 * The protocol versions the client speaks.
	 */
	protocol: ProtocolRange,
	/**
	 * Who is connecting.
	 */
	client: ClientInfo,
	/**
	 * What the client supports.
	 */
	capabilities: Capabilities,
};

/**
 * Capabilities by name, such as `{"agents": {}}`. Each value holds that capability's options,
 * and is empty when it has none. The map never changes shape.
 *
 * Later features are gated on a capability, so a newer editor still works with an older wispd.
 */
export type Capabilities = { [key in string]: { [key in string]: JsonValue } };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

/**
 * The client that sent `initialize`.
 */
export type ClientInfo = {
	/**
	 * The client's name, such as `wisp` for the editor.
	 */
	name: string,
	/**
	 * The client's release version.
	 */
	version: string,
	/**
	 * Tells apart the machines of one user, for the local runner (M5).
	 */
	machineId?: string,
};

/**
 * A range of protocol versions, both ends included. Its shape never changes.
 */
export type ProtocolRange = {
	/**
	 * The oldest version.
	 */
	min: number,
	/**
	 * The newest version.
	 */
	max: number,
};

/**
 * Result of `initialize`.
 */
export type InitializeResult = {
	/**
	 * The protocol version the connection speaks: the newest in both ranges.
	 */
	protocol: number,
	/**
	 * wispd's release version.
	 */
	wispd: string,
	/**
	 * Identifies wispd's event log. It changes only when the log starts over, and then every
	 * `seq` the client holds is meaningless.
	 */
	logId: LogId,
	/**
	 * What wispd supports.
	 */
	capabilities: Capabilities,
	/**
	 * The largest frame wispd accepts, in bytes, not counting the line ending.
	 */
	maxFrameBytes: number,
};

/**
 * Identifies wispd's event log. It changes only when the log starts over.
 */
export type LogId = string;

/**
 * Params of `host/health`.
 */
export type HostHealthParams = Record<symbol, never>;

/**
 * Result of `host/health`.
 */
export type HostHealthResult = {
	/**
	 * Seconds since wispd started.
	 */
	uptimeSeconds: number,
	/**
	 * Whether wispd can read and write its project store.
	 */
	store: StoreState,
	/**
	 * Agents running on this host. Always 0 before M3.
	 */
	runningAgents: number,
};

/**
 * The state of wispd's project store.
 *
 * A newer wispd may send states that are not listed here. Treat those as unknown, so a `switch`
 * over this type must not end in an exhaustiveness assertion.
 */
export type StoreState = "ok" | "unavailable";

/**
 * Params of `host/version`.
 */
export type HostVersionParams = Record<symbol, never>;

/**
 * Result of `host/version`.
 */
export type HostVersionResult = {
	/**
	 * wispd's release version.
	 */
	wispd: string,
	/**
	 * The protocol versions wispd speaks.
	 */
	protocol: ProtocolRange,
	/**
	 * The operating system and its version, such as `macOS 27.0`.
	 */
	os: string,
	/**
	 * The CPU architecture, such as `aarch64`.
	 */
	arch: string,
};

/**
 * Params of `project/list`.
 */
export type ProjectListParams = Record<symbol, never>;

/**
 * Result of `project/list`: a snapshot of every project.
 */
export type ProjectListResult = {
	/**
	 * Every project, oldest first.
	 */
	projects: Array<Project>,
	/**
	 * The `seq` of the last event the snapshot reflects. Subscribe with `after` set to it.
	 */
	seq: number,
};

/**
 * A project: a body of work on one repository, run by this wispd.
 */
export type Project = {
	/**
	 * The id the client chose when it created the project.
	 */
	id: ProjectId,
	/**
	 * The name shown in the editor.
	 */
	name: string,
	/**
	 * The absolute path of the repository on this host.
	 */
	repoPath: string,
	/**
	 * The branch checked out in the repository, or the first 7 digits of the commit when `HEAD`
	 * is detached, read when wispd sends the project. Absent when wispd can't read it.
	 */
	branch?: string,
	/**
	 * When the project was created, in RFC 3339 UTC.
	 */
	createdAt: string,
	/**
	 * When the project last changed, in RFC 3339 UTC.
	 */
	updatedAt: string,
};

/**
 * A project's id: a version 7 UUID that the client generates once and sends again on every
 * retry of `project/create`.
 */
export type ProjectId = string;

/**
 * Params of `project/create`.
 *
 * It is idempotent on `id`: if a project with that id exists, wispd returns it instead of
 * creating another, and fails with `idConflict` if `name` or `repoPath` differ. A new project's
 * `repoPath` must be the top folder of a git working tree on this host, or it fails with
 * `notARepository`.
 */
export type ProjectCreateParams = {
	/**
	 * The new project's id, a version 7 UUID generated by the client.
	 */
	id: ProjectId,
	/**
	 * The name shown in the editor.
	 */
	name: string,
	/**
	 * The absolute path of the repository on this host.
	 */
	repoPath: string,
};

/**
 * Result of `project/create`.
 */
export type ProjectCreateResult = {
	/**
	 * The project, new or existing.
	 */
	project: Project,
};

/**
 * Params of `events/subscribe`.
 *
 * wispd replays the events after `after`, then sends new ones as they happen, each as an
 * `events/event` notification. If those events are gone or too many to replay, it fails with
 * `resyncRequired`.
 */
export type EventsSubscribeParams = {
	/**
	 * Replay the events whose `seq` is greater than this: usually the `seq` of a snapshot, such
	 * as `project/list`'s, or of the last event received. `seq` starts at 1.
	 */
	after: number,
	/**
	 * Subscribe to one project's events. Without it, the subscription gets host-level events,
	 * such as `project.created`.
	 */
	project?: ProjectId,
};

/**
 * Result of `events/subscribe`.
 */
export type EventsSubscribeResult = {
	/**
	 * The new subscription, which its `events/event` notifications name.
	 */
	subscription: SubscriptionId,
};

/**
 * Identifies one `events/subscribe` on one connection. wispd generates it.
 */
export type SubscriptionId = string;

/**
 * Params of `events/unsubscribe`.
 */
export type EventsUnsubscribeParams = {
	/**
	 * The subscription to end. Ending one that does not exist succeeds.
	 */
	subscription: SubscriptionId,
};

/**
 * Result of `events/unsubscribe`.
 */
export type EventsUnsubscribeResult = Record<symbol, never>;

/**
 * Params of `accounts/keys/add`.
 *
 * It is idempotent on `id`: adding the same id again with the same `provider`, `label`, and
 * `key` returns the existing account instead of storing the key twice, and fails with
 * `idConflict` if they differ. The key is sent once; wispd stores it in the Keychain and never
 * echoes it back unmasked.
 */
export type AccountsKeysAddParams = {
	/**
	 * The new account's id, a version 7 UUID generated by the client.
	 */
	id: AccountId,
	/**
	 * Who the key is for.
	 */
	provider: Provider,
	/**
	 * A label the user chose, shown in the editor.
	 */
	label: string,
	/**
	 * The key. Never logged, never written anywhere but the Keychain, and never sent back.
	 */
	key: RawKey,
};

/**
 * A key account's id: a version 7 UUID that the client generates once and sends again on
 * every retry of `accounts/keys/add`.
 */
export type AccountId = string;

/**
 * Who a stored API key is for (0004's fallback for a subscription).
 *
 * A newer wispd may send providers that are not listed here. Treat those as unknown, so a
 * `switch` over this type must not end in an exhaustiveness assertion.
 */
export type Provider = "anthropic" | "openai" | "cursor";

/**
 * An API key on the wire.
 *
 * It serializes and deserializes as a plain string (serde's newtype-struct representation is
 * already transparent in JSON, so `#[serde(transparent)]` would be redundant here, and ts-rs 12
 * cannot parse it), but its `Debug` never shows the value, so a stray `{:?}` in a log line can't
 * leak it (#117). Compare wispd's `backend::ApiKey`, which does the same for the key once it
 * reaches a backend.
 */
export type RawKey = string;

/**
 * Result of `accounts/keys/add`.
 */
export type AccountsKeysAddResult = {
	/**
	 * The account, with its key masked.
	 */
	account: KeyAccount,
};

/**
 * A key account: its record in wisp's store. The key itself lives only in the macOS Keychain
 * (0004); wispd never returns it unmasked.
 */
export type KeyAccount = {
	/**
	 * The id the client chose when it added the key.
	 */
	id: AccountId,
	/**
	 * Who the key is for.
	 */
	provider: Provider,
	/**
	 * A label the user chose, shown in the editor.
	 */
	label: string,
	/**
	 * When the key was added, in RFC 3339 UTC.
	 */
	createdAt: string,
	/**
	 * The key, masked: its vendor prefix and last 4 characters, such as `sk-ant-...abcd`.
	 */
	maskedKey: string,
};

/**
 * Params of `accounts/keys/list`.
 */
export type AccountsKeysListParams = Record<symbol, never>;

/**
 * Result of `accounts/keys/list`.
 */
export type AccountsKeysListResult = {
	/**
	 * Every key account, oldest first, with its key masked.
	 */
	accounts: Array<KeyAccount>,
};

/**
 * Params of `accounts/keys/remove`.
 *
 * Removing an id that does not exist fails with `accountNotFound`.
 */
export type AccountsKeysRemoveParams = {
	/**
	 * The account to remove.
	 */
	id: AccountId,
};

/**
 * Result of `accounts/keys/remove`.
 */
export type AccountsKeysRemoveResult = Record<symbol, never>;

/**
 * Params of `accounts/list`.
 */
export type AccountsListParams = Record<symbol, never>;

/**
 * Result of `accounts/list`.
 */
export type AccountsListResult = {
	/**
	 * Every CLI wispd knows how to detect, in a stable order (`claude`, `codex`, `cursor`).
	 */
	clis: Array<DetectedCli>,
	/**
	 * When these results were read. `accounts/list` may answer from a short-lived cache;
	 * `accounts/refresh` always sets this to the time of a fresh probe.
	 */
	checkedAt: string,
};

/**
 * One CLI's detected state.
 *
 * Every field but `cli` and `installed` is best-effort: the status commands 0004 lists are
 * undocumented in places, so a field wispd could not read is `null` rather than a guess.
 */
export type DetectedCli = {
	/**
	 * Which CLI this is.
	 */
	cli: CliKind,
	/**
	 * Whether the binary resolves on the `PATH` wispd itself uses (#96).
	 */
	installed: boolean,
	/**
	 * The resolved absolute path, when installed.
	 */
	path?: string,
	/**
	 * The CLI's version, when the status command happened to expose it.
	 */
	version?: string,
	/**
	 * Whether the user is signed in. `null` when installed but wispd could not tell (a timeout,
	 * unparseable output, or an exit code 0004 doesn't document).
	 */
	signedIn?: boolean,
	/**
	 * How a signed-in CLI authenticates. Only set when `signedIn` is `true`.
	 */
	authKind?: AuthKind,
	/**
	 * The subscription plan or tier, where the status commands expose it (0004: undocumented
	 * for all three vendors).
	 */
	plan?: string,
	/**
	 * Why a field above is missing or uncertain, such as `"timed out after 5s"`. Never set on a
	 * clean read.
	 */
	note?: string,
};

/**
 * How a signed-in CLI authenticates.
 *
 * A newer wispd may send kinds that are not listed here. Treat those as unknown, so a `switch`
 * over this type must not end in an exhaustiveness assertion.
 */
export type AuthKind = "subscription" | "apiKey";

/**
 * A vendor CLI wispd knows how to detect (0004).
 *
 * A newer wispd may send kinds that are not listed here. Treat those as unknown, so a `switch`
 * over this type must not end in an exhaustiveness assertion.
 */
export type CliKind = "claude" | "codex" | "cursor";

/**
 * Params of `accounts/refresh`.
 */
export type AccountsRefreshParams = Record<symbol, never>;

/**
 * Result of `accounts/refresh`.
 */
export type AccountsRefreshResult = {
	/**
	 * Every CLI wispd knows how to detect, freshly probed.
	 */
	clis: Array<DetectedCli>,
	/**
	 * When this probe ran.
	 */
	checkedAt: string,
};

/**
 * Params of `usage/get`.
 *
 * Empty: wispd has no account registry yet (#114, #117, #118 are still open, and #113's
 * `AccountRef` is already just a caller-supplied string), so it reports every account id it has
 * recorded usage or limits for.
 */
export type UsageGetParams = Record<symbol, never>;

/**
 * Result of `usage/get`.
 */
export type UsageGetResult = {
	/**
	 * Every account wispd has recorded usage or limits for, in no particular order.
	 */
	accounts: Array<AccountUsage>,
};

/**
 * One account's usage and latest limit windows.
 */
export type AccountUsage = {
	/**
	 * wispd's id for the account (#114, #117).
	 */
	accountId: string,
	/**
	 * Tokens and cost used today, local time on this host.
	 */
	today: UsagePeriod,
	/**
	 * Tokens and cost used this week (Monday to now), local time on this host.
	 */
	week: UsagePeriod,
	/**
	 * The account's limit windows, as last reported. Empty when the vendor reports none (0004:
	 * Cursor's headless output has no usage API).
	 */
	limits: Array<UsageLimitWindow>,
};

/**
 * One of an account's limit windows, as a vendor last reported it (0004's `rate_limit_event` and
 * Codex's `account/rateLimits/read`).
 */
export type UsageLimitWindow = {
	/**
	 * The vendor's name for the window, such as `five_hour`, `seven_day`, `primary`, or
	 * `secondary`.
	 */
	window: string,
	/**
	 * How much of the window is used, from 0 to 100, when the vendor says.
	 */
	usedPercent?: number | null,
	/**
	 * When the window resets, when the vendor says.
	 */
	resetsAt?: string | null,
	/**
	 * When wispd captured this snapshot.
	 */
	capturedAt: string,
};

/**
 * Tokens and cost over a period.
 */
export type UsagePeriod = {
	/**
	 * Input tokens, not counting cache reads and writes.
	 */
	inputTokens: number,
	/**
	 * Output tokens, including reasoning.
	 */
	outputTokens: number,
	/**
	 * Input tokens read from the prompt cache.
	 */
	cacheReadTokens: number,
	/**
	 * Input tokens written to the prompt cache.
	 */
	cacheWriteTokens: number,
	/**
	 * The cost, when the vendor reports one for this account in the period. Absent, not zero,
	 * when it never does (0004: Codex and Cursor report no cost).
	 */
	costUsdMicros?: number | null,
};

/**
 * Params of `accounts/defaults/get`.
 */
export type AccountsDefaultsGetParams = Record<symbol, never>;

/**
 * Result of `accounts/defaults/get`, and of `accounts/defaults/set`: this host's default account
 * for each role, absent where none is set.
 */
export type AccountsDefaultsGetResult = {
	/**
	 * The coordinator's default account.
	 */
	coordinator?: AccountChoice,
	/**
	 * A worker's default account.
	 */
	worker?: AccountChoice,
};

/**
 * An account a task can be routed to: the user's own login in a vendor CLI, or a stored key.
 *
 * A newer wispd may send a kind this version does not know. Treat that as absent rather than end
 * a `switch` over this type in an exhaustiveness assertion.
 */
export type AccountChoice = { "kind": "subscription",
	/**
	 * The backend's name, as `host/health`'s running agents and wispd's logs use it.
	 */
	backend: string, } | { "kind": "key",
	/**
	 * The key account's id.
	 */
	id: AccountId,
};

/**
 * Params of `accounts/defaults/set`.
 */
export type AccountsDefaultsSetParams = {
	/**
	 * The role to set the default for.
	 */
	role: Role,
	/**
	 * The new default, or `None` to clear it.
	 */
	account?: AccountChoice,
};

/**
 * Which kind of run an account is chosen for (0004).
 */
export type Role = "coordinator" | "worker";

/**
 * Params of `context/list`.
 */
export type ContextListParams = {
	/**
	 * The project whose shared context to list.
	 */
	project: ProjectId,
};

/**
 * Result of `context/list`.
 */
export type ContextListResult = {
	/**
	 * Every file in the project's shared context, ordered by path.
	 */
	files: Array<ContextFile>,
};

/**
 * One shared context file, without its content.
 */
export type ContextFile = {
	/**
	 * The file's name, relative to the project's context folder.
	 */
	path: string,
	/**
	 * Its size in bytes.
	 */
	size: number,
	/**
	 * When it was last modified, in RFC 3339 UTC.
	 */
	modifiedAt: string,
	/**
	 * Who wrote it last, such as `"editor"` or an agent's run id, when wispd knows. Absent for a
	 * file wispd has not seen written since it started, such as one already on disk at startup.
	 */
	lastWriter?: string,
};

/**
 * Params of `context/read`.
 */
export type ContextReadParams = {
	/**
	 * The file's project.
	 */
	project: ProjectId,
	/**
	 * The file's path. Fails with `contextNotFound` if it does not exist.
	 */
	path: string,
};

/**
 * Result of `context/read`.
 */
export type ContextReadResult = {
	/**
	 * The file's metadata.
	 */
	file: ContextFile,
	/**
	 * Its content.
	 */
	content: string,
};

/**
 * Params of `context/write`.
 *
 * It is idempotent on `id`: writing the same id again with the same `project`, `path`,
 * `content`, and `writer` returns the same result instead of writing twice or emitting a second
 * `context.changed` event; with different params it fails with `idConflict`. A write to a path
 * with existing content replaces it; the last write to a path wins when two race (0005).
 */
export type ContextWriteParams = {
	/**
	 * The write's id, a version 7 UUID generated by the client.
	 */
	id: ContextWriteId,
	/**
	 * The file's project.
	 */
	project: ProjectId,
	/**
	 * The file's path.
	 */
	path: string,
	/**
	 * The file's new content, in full: `context/write` replaces a file, it does not patch one.
	 */
	content: string,
	/**
	 * A label for who is writing, such as `"editor"` or an agent's run id, shown later in
	 * `context/list` and in the `context.changed` event as `lastWriter`. Omitted when the caller
	 * has none to give.
	 */
	writer?: string,
};

/**
 * A `context/write`'s id: a version 7 UUID that the client generates once and sends again on
 * every retry, so a retry never applies the write twice or emits a duplicate `context.changed`
 * event.
 */
export type ContextWriteId = string;

/**
 * Result of `context/write`.
 */
export type ContextWriteResult = {
	/**
	 * The written file's metadata.
	 */
	file: ContextFile,
};

/**
 * Params of `agent/start`.
 *
 * Idempotent on `runId`: starting the same id again with the same params returns the run
 * instead of starting another; with different params it fails with `idConflict`.
 */
export type AgentStartParams = {
	/**
	 * The new run's id, a version 7 UUID generated by the client.
	 */
	runId: RunId,
	/**
	 * The project whose repository the run works in.
	 */
	project: ProjectId,
	/**
	 * The task.
	 */
	prompt: string,
	/**
	 * What the run's tools may do. Only `workspaceWrite` exists.
	 */
	policy: AgentPolicy,
	/**
	 * The account to run on. Absent means the worker role's default (`accounts/defaults/*`).
	 */
	account?: AccountChoice,
};

/**
 * What a run's tools may do. Only workers run through `agent/start`.
 *
 * A newer wispd may send a policy this version does not know; treat it as unknown.
 */
export type AgentPolicy = "workspaceWrite";

/**
 * An agent run's id: a version 7 UUID that the client generates once and sends again on every
 * retry of `agent/start`, so a retry never starts a second agent.
 */
export type RunId = string;

/**
 * Result of `agent/start`, `agent/send`, and `agent/cancel`: the run as it stands.
 */
export type AgentRunResult = {
	/**
	 * The run.
	 */
	run: AgentRun,
};

/**
 * One agent run.
 */
export type AgentRun = {
	/**
	 * The run's id.
	 */
	id: RunId,
	/**
	 * Its project.
	 */
	project: ProjectId,
	/**
	 * The task it was started with.
	 */
	prompt: string,
	/**
	 * What its tools may do.
	 */
	policy: AgentPolicy,
	/**
	 * Where it is.
	 */
	status: AgentStatus,
	/**
	 * The backend running it, such as `claude`.
	 */
	backend: string,
	/**
	 * The account it is charged to now: the backend's name for a subscription, or a key
	 * account's id. It changes on `agent.accountFallback`.
	 */
	accountId: string,
	/**
	 * Its worktree's branch, such as `wisp/1a2b3c4d`, once the worktree exists.
	 */
	branch?: string,
	/**
	 * Its worktree's absolute path on the host, once it exists.
	 */
	worktreePath?: string,
	/**
	 * The vendor's session id, once the CLI reported it.
	 */
	sessionId?: string,
	/**
	 * Why it failed, for people.
	 */
	error?: string,
	/**
	 * Its latest commit, once wispd made one.
	 */
	diff?: DiffSummary,
	/**
	 * When it was created, in RFC 3339 UTC.
	 */
	createdAt: string,
	/**
	 * When it last changed, in RFC 3339 UTC.
	 */
	updatedAt: string,
};

/**
 * Where a run is.
 *
 * A newer wispd may send a status this version does not know; treat it as unknown, and don't
 * end a `switch` over this type in an exhaustiveness assertion.
 */
export type AgentStatus = "starting" | "running" | "completed" | "failed" | "cancelled" | "interrupted" | "accepted";

/**
 * The commit wispd made for a run, compared with the commit its worktree was created from.
 */
export type DiffSummary = {
	/**
	 * The commit's full sha, on the run's branch.
	 */
	commit: string,
	/**
	 * Files changed.
	 */
	files: number,
	/**
	 * Lines added.
	 */
	insertions: number,
	/**
	 * Lines removed.
	 */
	deletions: number,
};

/**
 * Params of `agent/send`: a message to a run (0011).
 *
 * A running agent gets it as its next turn. An agent that ended with a `sessionId` resumes
 * that session in its worktree, with the message as the prompt. Idempotent on `turnId` while
 * wispd runs.
 */
export type AgentSendParams = {
	/**
	 * The run.
	 */
	runId: RunId,
	/**
	 * The message's id, a version 7 UUID generated by the client.
	 */
	turnId: TurnId,
	/**
	 * The message.
	 */
	text: string,
};

/**
 * A follow-up turn's id: a version 7 UUID that the client generates once and sends again on
 * every retry of `agent/send`, so a retry never sends the message twice.
 */
export type TurnId = string;

/**
 * Params of `agent/cancel`: stops a running agent, which ends as `cancelled`. Cancelling a run
 * that isn't running does nothing.
 */
export type AgentCancelParams = {
	/**
	 * The run.
	 */
	runId: RunId,
};

/**
 * Params of `agent/list`.
 */
export type AgentListParams = {
	/**
	 * Only this project's runs. Absent lists every run on the host.
	 */
	project?: ProjectId,
};

/**
 * Result of `agent/list`.
 */
export type AgentListResult = {
	/**
	 * The runs, oldest first.
	 */
	runs: Array<AgentRun>,
	/**
	 * The `seq` of the last event the list reflects, to subscribe after.
	 */
	seq: number,
};

/**
 * Params of `agent/events`: one run's events from wispd's log, for rebuilding its transcript
 * after `resyncRequired` or a restart.
 */
export type AgentEventsParams = {
	/**
	 * The run.
	 */
	runId: RunId,
	/**
	 * Return the events whose `seq` is greater than this; 0 for the first page.
	 */
	after: number,
	/**
	 * The most events to return: 500 by default, and at most 1000.
	 */
	limit?: number,
};

/**
 * Result of `agent/events`.
 */
export type AgentEventsResult = {
	/**
	 * The events, oldest first.
	 */
	events: Array<LoggedEvent>,
	/**
	 * Whether more events follow the last one returned. Ask again after its `seq`.
	 */
	more: boolean,
};

/**
 * An event from wispd's log, as `agent/events` returns it.
 */
export type LoggedEvent = {
	/**
	 * The event's position in the log.
	 */
	seq: number,
	/**
	 * When it happened, in RFC 3339 UTC.
	 */
	time: string,
	/**
	 * Its project. Absent for host-level events.
	 */
	project?: ProjectId,
	/**
	 * What happened.
	 */
	event: WispEvent,
};

/**
 * What happened, by `kind`.
 *
 * A newer wispd may send kinds that are not listed here. Skip those events but still count their
 * `seq` as received, and don't end a `switch` over this type in an exhaustiveness assertion.
 */
export type WispEvent = { "kind": "project.created",
	/**
	 * The new project.
	 */
	project: Project, } | { "kind": "context.changed",
	/**
	 * The changed file.
	 */
	file: ContextFile, } | { "kind": "agent.started",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * The run as it was created. wispd always sends it.
	 */
	run?: AgentRun, } | { "kind": "agent.updated",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * The run's changing fields as they stand now.
	 */
	state: AgentRunState, } | { "kind": "agent.output",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * What happened, in order.
	 */
	items: Array<AgentOutputItem>, } | { "kind": "agent.accountFallback",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * The account it was on.
	 */
	fromAccount: string,
	/**
	 * The account it is on now.
	 */
	toAccount: string,
	/**
	 * Why it moved.
	 */
	reason: AgentFailureKind, } | { "kind": "agent.finished",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * How it ended.
	 */
	outcome: AgentOutcome, } | { "kind": "agent.diffReady",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * The commit and its stats against the worktree's base.
	 */
	diff: DiffSummary, } | { "kind": "agent.accepted",
	/**
	 * The run's id.
	 */
	runId: RunId,
	/**
	 * What happened to the project's repository.
	 */
	merge: AgentMerge, } | { "kind": "repo.added",
	/**
	 * The entry.
	 */
	repo: Repo, } | { "kind": "thread.started",
	/**
	 * The thread.
	 */
	thread: Thread, } | { "kind": "thread.updated",
	/**
	 * The thread as it stands.
	 */
	thread: Thread, } | { "kind": "thread.deleted",
	/**
	 * The thread's run id.
	 */
	runId: RunId,
	/**
	 * Its repo entry.
	 */
	repo: RepoId,
};

/**
 * Why a run failed, for code to match on.
 *
 * A newer wispd may send a kind this version does not know; treat it as unknown.
 */
export type AgentFailureKind = "notSignedIn" | "rateLimited" | "policyViolation" | "unexpectedApiKey" | "vendorError" | "crashed" | "spawnFailed" | "commitFailed" | "internal";

/**
 * What `agent/accept` did to the project's repository.
 */
export type AgentMerge = {
	/**
	 * The commit the branch points at now: the run's commit, or wispd's merge commit.
	 */
	commit: string,
	/**
	 * The branch it went into, such as `main`: the repository's current branch when the run
	 * was accepted.
	 */
	into: string,
	/**
	 * How.
	 */
	how: AgentMergeKind,
};

/**
 * How `agent/accept` brought a run's commit into the project's branch.
 *
 * A newer wispd may send a value this version does not know; treat it as unknown.
 */
export type AgentMergeKind = "fastForward" | "merge" | "upToDate";

/**
 * How one CLI process of a run ended.
 *
 * A newer wispd may send a status this version does not know; treat it as unknown.
 */
export type AgentOutcome = { "status": "completed",
	/**
	 * The last turn's final text, when the vendor reports one.
	 */
	result?: string, } | { "status": "cancelled" } | { "status": "failed",
	/**
	 * What went wrong.
	 */
	failure: AgentFailureKind,
	/**
	 * A description for people.
	 */
	message: string, } | { "status": "interrupted"
};

/**
 * One thing in a run's transcript: a normalized event from its CLI (0004), by `kind`.
 *
 * A newer wispd may send kinds that are not listed here; skip them.
 */
export type AgentOutputItem = { "kind": "sessionStarted",
	/**
	 * The vendor's session id.
	 */
	sessionId: string,
	/**
	 * The model, when the CLI says.
	 */
	model?: string, } | { "kind": "turnStarted",
	/**
	 * The turn's id, when the caller gave one.
	 */
	turnId?: TurnId, } | { "kind": "textDelta",
	/**
	 * The vendor's id for the message, when it has one.
	 */
	messageId?: string,
	/**
	 * The new text.
	 */
	text: string, } | { "kind": "text",
	/**
	 * The vendor's id for the message, when it has one.
	 */
	messageId?: string,
	/**
	 * The text.
	 */
	text: string, } | { "kind": "toolCall",
	/**
	 * The call's id, which its `toolResult` repeats.
	 */
	callId: string,
	/**
	 * The tool, in the vendor's naming, such as `Edit`.
	 */
	name: string,
	/**
	 * The tool's input as the vendor sent it, or `{"truncated": true, "bytes": n}` when it
	 * was too large to forward.
	 */
	input: JsonValue, } | { "kind": "toolResult",
	/**
	 * The call's id.
	 */
	callId: string,
	/**
	 * How it ended.
	 */
	status: AgentToolStatus,
	/**
	 * What the tool returned, when the vendor includes it, cut short when it is long.
	 */
	output?: string, } | { "kind": "reasoning",
	/**
	 * The vendor's id for the message, when it has one.
	 */
	messageId?: string,
	/**
	 * The text.
	 */
	text: string, } | { "kind": "todoList",
	/**
	 * The items, in order.
	 */
	items: Array<AgentTodoItem>, } | { "kind": "notice",
	/**
	 * The message.
	 */
	detail: string, } | { "kind": "turnFinished",
	/**
	 * The turn's id, as its `turnStarted` had it.
	 */
	turnId?: TurnId,
	/**
	 * The turn's final text, when the vendor reports one.
	 */
	result?: string, } | { "kind": "followUpDropped",
	/**
	 * The follow-up's turn id.
	 */
	turnId: TurnId, } | { "kind": "usage",
	/**
	 * The model, when the vendor breaks usage down by model.
	 */
	model?: string,
	/**
	 * Input tokens, not counting cache reads and writes.
	 */
	inputTokens: number,
	/**
	 * Output tokens.
	 */
	outputTokens: number,
	/**
	 * Input tokens read from the prompt cache.
	 */
	cacheReadTokens: number,
	/**
	 * Input tokens written to the prompt cache.
	 */
	cacheWriteTokens: number,
	/**
	 * The cost the vendor reported, in millionths of a US dollar, when it reports one.
	 */
	costUsdMicros?: number, } | { "kind": "warning",
	/**
	 * A short description.
	 */
	detail: string,
};

/**
 * One item of an agent's checklist.
 */
export type AgentTodoItem = {
	/**
	 * What to do.
	 */
	text: string,
	/**
	 * How far along it is.
	 */
	status: AgentTodoStatus,
};

/**
 * How far along a checklist item is.
 *
 * A newer wispd may send a status this version does not know; treat it as unknown.
 */
export type AgentTodoStatus = "pending" | "inProgress" | "completed";

/**
 * How a tool call ended.
 *
 * A newer wispd may send a status this version does not know; treat it as unknown.
 */
export type AgentToolStatus = "ok" | "error" | "denied";

/**
 * The part of a run that changes while it runs, as `agent.updated` reports it. The rest of
 * [`AgentRun`], including its prompt, never changes after `agent.started`.
 */
export type AgentRunState = {
	/**
	 * Where the run is.
	 */
	status: AgentStatus,
	/**
	 * The account it is charged to now.
	 */
	accountId: string,
	/**
	 * The vendor's session id, once the CLI reported it.
	 */
	sessionId?: string,
	/**
	 * Why it failed, for people. Absent once it runs again.
	 */
	error?: string,
	/**
	 * Its latest commit, once wispd made one.
	 */
	diff?: DiffSummary,
	/**
	 * When it changed, in RFC 3339 UTC.
	 */
	updatedAt: string,
};

/**
 * A repository on the host that normal threads run in.
 */
export type Repo = {
	/**
	 * The entry's id.
	 */
	id: RepoId,
	/**
	 * The name shown in the editor: the repository folder's name.
	 */
	name: string,
	/**
	 * The absolute, canonical path of the repository on the host.
	 */
	path: string,
	/**
	 * True for wispd's scratch entry, which holds the threads with no repo. Its `path` is the
	 * folder that holds each such thread's own scratch repository.
	 */
	scratch?: boolean,
	/**
	 * When the entry was created, in RFC 3339 UTC.
	 */
	createdAt: string,
};

/**
 * A repo entry's id: a version 7 UUID that the client generates once and sends again on
 * every retry of `repo/add`. wispd generates the scratch entry's.
 */
export type RepoId = string;

/**
 * A normal thread: what threads add to its run.
 */
export type Thread = {
	/**
	 * The thread's run id.
	 */
	id: RunId,
	/**
	 * Its repo entry: the scratch entry for a thread with no repo.
	 */
	repo: RepoId,
	/**
	 * Whether the user archived it.
	 */
	archived?: boolean,
	/**
	 * When it was created, in RFC 3339 UTC.
	 */
	createdAt: string,
};

/**
 * Params of `agent/diff`.
 */
export type AgentDiffParams = {
	/**
	 * The run.
	 */
	runId: RunId,
};

/**
 * Result of `agent/diff`.
 */
export type AgentDiffResult = {
	/**
	 * The commit the run's worktree was created from: the `base` side.
	 */
	base: string,
	/**
	 * The run's latest commit: the `head` side, and what `agent/accept` merges. Equal to `base`
	 * until wispd has committed something for the run.
	 */
	head: string,
	/**
	 * The files that differ, ordered by path.
	 */
	files: Array<AgentDiffFile>,
	/**
	 * Totals over every changed file.
	 */
	stats: AgentDiffStats,
	/**
	 * Whether `files` was cut short because the run changed more files than one answer lists.
	 */
	truncated: boolean,
};

/**
 * One file that differs between the run's base and its latest commit.
 */
export type AgentDiffFile = {
	/**
	 * Its path on the head side, relative to the repository root. For a deleted file, its path
	 * on the base side.
	 */
	path: string,
	/**
	 * Its path on the base side, for a rename or a copy.
	 */
	oldPath?: string,
	/**
	 * How it changed.
	 */
	status: AgentFileStatus,
	/**
	 * Lines added. 0 for a binary file.
	 */
	insertions: number,
	/**
	 * Lines removed. 0 for a binary file.
	 */
	deletions: number,
	/**
	 * Whether git treats it as binary. A binary file has no `diff`.
	 */
	binary: boolean,
	/**
	 * Its unified diff, starting at its `diff --git` line. Absent for a binary file, and for
	 * every file after the result's diffs reached their total size cap; read those files with
	 * `agent/file` instead.
	 */
	diff?: string,
	/**
	 * Whether `diff` was cut short at the per-file size cap.
	 */
	diffTruncated: boolean,
};

/**
 * How a file differs from the base.
 *
 * A newer wispd may send a status this version does not know; treat it as modified.
 */
export type AgentFileStatus = "added" | "modified" | "deleted" | "renamed" | "copied" | "typeChanged";

/**
 * Totals of an `agent/diff`, over every file, including files left out of `files`.
 */
export type AgentDiffStats = {
	/**
	 * Files changed.
	 */
	files: number,
	/**
	 * Lines added.
	 */
	insertions: number,
	/**
	 * Lines removed.
	 */
	deletions: number,
};

/**
 * Params of `agent/file`.
 */
export type AgentFileParams = {
	/**
	 * The run.
	 */
	runId: RunId,
	/**
	 * The file's path, relative to the repository root, as `agent/diff` lists it: no leading
	 * `/`, no `.` or `..` or empty component, no backslash, and nothing under `.git`.
	 */
	path: string,
	/**
	 * Which side to read.
	 */
	side: AgentFileSide,
	/**
	 * Whether to leave the content out and answer only `exists` and `size`, as a file system's
	 * `stat` needs. Absent means false.
	 */
	sizeOnly?: boolean,
};

/**
 * Which side of a run's diff to read.
 *
 * A newer client may send a side this version does not know; wispd refuses it.
 */
export type AgentFileSide = "base" | "head";

/**
 * Result of `agent/file`.
 */
export type AgentFileResult = {
	/**
	 * The path as asked.
	 */
	path: string,
	/**
	 * The side as asked.
	 */
	side: AgentFileSide,
	/**
	 * The commit it was read from.
	 */
	commit: string,
	/**
	 * Whether the file exists on that side. An added file has no base side, and a deleted file
	 * no head side.
	 */
	exists: boolean,
	/**
	 * Its size in bytes, when it exists.
	 */
	size?: number,
	/**
	 * Its exact content, base64-encoded, when it exists, is not too large, and `sizeOnly` was not
	 * asked. A symlink's
	 * content is its target, as git stores it; it is never followed.
	 */
	content?: string,
	/**
	 * Whether it is over `agent/file`'s size cap, so `content` is absent.
	 */
	tooLarge: boolean,
};

/**
 * Params of `agent/accept`.
 *
 * Idempotent on `id`: accepting an accepted run again with the same id returns the same result;
 * with another id it fails with `runAccepted`.
 */
export type AgentAcceptParams = {
	/**
	 * The run.
	 */
	runId: RunId,
	/**
	 * The accept's id, a version 7 UUID generated by the client.
	 */
	id: AcceptId,
	/**
	 * The commit the user reviewed, `agent/diff`'s `head`. When it is given and the run has
	 * committed since, the accept fails with `mergeRefused` instead of merging changes nobody
	 * reviewed.
	 */
	commit?: string,
};

/**
 * An `agent/accept`'s id: a version 7 UUID that the client generates once and sends again on
 * every retry, so a retry after a lost connection gets the same answer.
 */
export type AcceptId = string;

/**
 * Result of `agent/accept`.
 */
export type AgentAcceptResult = {
	/**
	 * The run, now `accepted`.
	 */
	run: AgentRun,
	/**
	 * What happened to the project's repository.
	 */
	merge: AgentMerge,
};

/**
 * Params of `agent/requestChanges`: the reviewer's follow-up to a run, sent to it as
 * `agent/send` sends a message, and idempotent on `turnId` the same way.
 */
export type AgentRequestChangesParams = {
	/**
	 * The run.
	 */
	runId: RunId,
	/**
	 * The message's id, a version 7 UUID generated by the client.
	 */
	turnId: TurnId,
	/**
	 * What to change.
	 */
	text: string,
};

/**
 * Params of `thread/list`.
 */
export type ThreadListParams = Record<symbol, never>;

/**
 * Result of `thread/list`: every repo entry and every thread. The threads' runs come from
 * `agent/list` with each entry's id.
 */
export type ThreadListResult = {
	/**
	 * Every repo entry, oldest first.
	 */
	repos: Array<Repo>,
	/**
	 * Every thread, oldest first.
	 */
	threads: Array<Thread>,
	/**
	 * The `seq` of the last event the snapshot reflects. Subscribe to host-level events with
	 * `after` set to it.
	 */
	seq: number,
};

/**
 * Params of `repo/add`: registers a repository for normal threads.
 *
 * Idempotent on `id`, and failing with `idConflict` if the same id comes with another path. A
 * path that is already registered returns its existing entry, whatever `id` says. The path must
 * be the top folder of a git working tree on the host, or it fails with `notARepository`.
 */
export type RepoAddParams = {
	/**
	 * The new entry's id, a version 7 UUID generated by the client.
	 */
	id: RepoId,
	/**
	 * The absolute path of the repository on the host.
	 */
	path: string,
};

/**
 * Result of `repo/add`.
 */
export type RepoAddResult = {
	/**
	 * The entry, new or existing.
	 */
	repo: Repo,
};

/**
 * Params of `thread/start`: starts a normal thread's agent in its own worktree, as `agent/start`
 * starts a worker, with the same sandbox (0013).
 *
 * Idempotent on `runId`: the same id with the same params returns the run; with different
 * params it fails with `idConflict`.
 */
export type ThreadStartParams = {
	/**
	 * The new run's id, a version 7 UUID generated by the client.
	 */
	runId: RunId,
	/**
	 * The repo entry to work in. Absent starts a thread with no repo, in a scratch repository
	 * of its own.
	 */
	repo?: RepoId,
	/**
	 * The first message.
	 */
	prompt: string,
	/**
	 * The account to run on. Absent means the worker role's default (`accounts/defaults/*`).
	 */
	account?: AccountChoice,
};

/**
 * Result of `thread/start`.
 */
export type ThreadStartResult = {
	/**
	 * The thread.
	 */
	thread: Thread,
	/**
	 * Its run, as it stands.
	 */
	run: AgentRun,
};

/**
 * Params of `thread/archive`: archives a thread or brings it back.
 */
export type ThreadArchiveParams = {
	/**
	 * The thread's run id.
	 */
	runId: RunId,
	/**
	 * True to archive it, false to bring it back.
	 */
	archived: boolean,
};

/**
 * Result of `thread/archive`.
 */
export type ThreadArchiveResult = {
	/**
	 * The thread as it stands.
	 */
	thread: Thread,
};

/**
 * Params of `thread/delete`: deletes a thread, its run, its worktree and branch, a thread with
 * no repo's scratch repository, and its stored events.
 *
 * A running CLI is cancelled first, and the delete answers once it has exited and its changes
 * were committed. Deleting a thread that doesn't exist fails with `threadNotFound`.
 */
export type ThreadDeleteParams = {
	/**
	 * The thread's run id.
	 */
	runId: RunId,
};

/**
 * Result of `thread/delete`.
 */
export type ThreadDeleteResult = Record<symbol, never>;

/**
 * Params of `$/cancelRequest`.
 */
export type CancelRequestParams = {
	/**
	 * The id of the request to cancel.
	 */
	id: RequestId,
};

/**
 * A request id, chosen by the sender: an integer or a string.
 */
export type RequestId = number | string;

/**
 * Params of `events/event`: one event from wispd's event log.
 */
export type EventsEventParams = {
	/**
	 * The subscription it belongs to.
	 */
	subscription: SubscriptionId,
	/**
	 * The event's position in wispd's log. It increases by one for every change on the host,
	 * across all projects.
	 */
	seq: number,
	/**
	 * When the change happened, in RFC 3339 UTC.
	 */
	time: string,
	/**
	 * The project it belongs to. Absent for host-level events.
	 */
	project?: ProjectId,
	/**
	 * What happened.
	 */
	event: WispEvent,
};

/**
 * The `data` of a wisp error (code -32000).
 */
export type ErrorData = {
	/**
	 * What went wrong.
	 */
	kind: ErrorKind,
	/**
	 * More about it, in a shape that depends on `kind`.
	 */
	detail?: JsonValue,
};

/**
 * What went wrong, in a wisp error's `data.kind`. Receivers match on it, never on the message.
 *
 * A newer wispd may send kinds that are not listed here. Treat those as unknown errors, so a
 * `switch` over this type must not end in an exhaustiveness assertion.
 */
export type ErrorKind = "notInitialized" | "incompatibleProtocol" | "resyncRequired" | "projectNotFound" | "accountNotFound" | "keychainUnavailable" | "idConflict" | "contextNotFound" | "contextTooLarge" | "notARepository" | "runNotFound" | "runNotResumable" | "workerUnavailable" | "worktreeFailed" | "runAccepted" | "mergeRefused" | "mergeConflict" | "repoNotFound" | "threadNotFound";

/**
 * The `detail` of `incompatibleProtocol`. Its shape never changes, so every editor can read it
 * from every wispd.
 */
export type IncompatibleProtocolDetail = {
	/**
	 * The versions the client asked for.
	 */
	requested: ProtocolRange,
	/**
	 * The versions wispd speaks.
	 */
	supported: ProtocolRange,
	/**
	 * wispd's release version, so the editor can say which side to update.
	 */
	wispd: string,
};
