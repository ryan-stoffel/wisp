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
	file: ContextFile,
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
export type ErrorKind = "notInitialized" | "incompatibleProtocol" | "resyncRequired" | "projectNotFound" | "accountNotFound" | "keychainUnavailable" | "idConflict" | "contextNotFound" | "contextTooLarge" | "notARepository";

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
