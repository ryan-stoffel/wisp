// One agent run's transcript, rebuilt from its logged events. Pure, so it is
// tested without React; useAgentRun feeds it pages of `agent/events` and live
// events alike.
import type {
  AgentFailureKind,
  AgentOutcome,
  AgentOutputItem,
  AgentRun,
  AgentStatus,
  AgentTodoItem,
  AgentToolStatus,
  ImageId,
  JsonValue,
  LoggedEvent,
  ParallaxEvent,
} from "../protocol/generated/protocol";

/** One row of the transcript. `key` is stable across re-renders; `at` is when it began. */
export type Item = ItemBody & { at?: string };

type ItemBody =
  /**
   * `text` is null for a follow-up logged by a plxd from before it recorded the text. `wake` marks
   * a turn plxd sent a coordinator itself, when runs it started finished (0025). `from` is the run
   * id of the thread that sent it with its Parallax tools, not the user (0041). `images` are the
   * ids of the images sent with it, for `agent/image` (RYA-193).
   */
  | {
      kind: "user";
      key: string;
      text: string | null;
      turnId?: string;
      wake?: boolean;
      from?: string;
      images?: ImageId[];
    }
  /** `partial` while it is still arriving as `textDelta`s. */
  | { kind: "assistant"; key: string; text: string; messageId?: string; partial?: boolean }
  | { kind: "reasoning"; key: string; text: string }
  /**
   * `status` is absent until its result arrives; `name` is null for a result with no call.
   * `subagent` is the first line of the prompt of the subagent a coordinator's plxd tool names.
   */
  | {
      kind: "tool";
      key: string;
      callId: string;
      name: string | null;
      input?: JsonValue;
      status?: AgentToolStatus;
      output?: string;
      subagent?: string;
    }
  /** `active` is the step under way as the agent words it, from Claude Code's task tools. */
  | { kind: "todo"; key: string; items: AgentTodoItem[]; active?: string }
  /**
   * `turnId` marks a follow-up that never reached the agent. `from` is the run id of the thread
   * that stopped the run (0041).
   */
  | {
      kind: "notice";
      key: string;
      tone: "info" | "warning";
      text: string;
      turnId?: string;
      from?: string;
    }
  /**
   * A permission request (RYA-196, 0031): what the agent asks to do, and how it ended, which is
   * absent while it waits.
   */
  | { kind: "approval"; key: string; request: ApprovalRequest; resolved?: ApprovalResolution }
  /** How one CLI process of the run ended. */
  | { kind: "end"; key: string; outcome: AgentOutcome }
  /**
   * Where a CLI process started or resumed its session, by the vendor's id. Never shown:
   * `withTaskLists` takes it out, and starts a new task list when the id changes (RYA-250).
   */
  | { kind: "session"; key: string; sessionId: string };

/** A permission request as `approvalRequested` carries it. */
export type ApprovalRequest = Omit<Extract<AgentOutputItem, { kind: "approvalRequested" }>, "kind">;

/**
 * How a permission request ended, as `approvalResolved` or `agent/approve` says, and when. `gone`
 * is this app's own: `agent/approve` found no such request, so it ended without saying how here.
 */
export type ApprovalResolution = Omit<
  Extract<AgentOutputItem, { kind: "approvalResolved" }>,
  "kind" | "approvalId"
> & { at?: string; gone?: boolean };

export type Approval = Extract<Item, { kind: "approval" }>;

export interface Transcript {
  /** Absent until `agent.started` is in. */
  run?: AgentRun;
  items: Item[];
  /** The last `seq` applied. Anything at or below it is a repeat. */
  seq: number;
}

export const emptyTranscript: Transcript = { items: [], seq: 0 };

/**
 * Applies events, in `seq` order, for one run. Repeats and other runs' events are
 * skipped, and kinds this version doesn't know still count their `seq`.
 */
export function applyEvents(t: Transcript, events: LoggedEvent[], runId: string): Transcript {
  let { run, seq } = t;
  const items = [...t.items];
  const push = (item: Item) => items.push(item);

  for (const { seq: at, time, event } of events) {
    if (at <= seq) continue;
    seq = at;
    if (!("runId" in event) || event.runId !== runId) continue;
    const key = (i: number | string = 0) => `${at}:${i}`;
    const before = items.length;

    run = updateRun(run, event);
    switch (event.kind) {
      case "agent.started":
        if (event.run) push({ kind: "user", key: key(), text: event.run.prompt });
        break;
      case "agent.accountFallback":
        push({
          kind: "notice",
          key: key(),
          tone: "info",
          text: `Switched from ${accountLabel(event.fromAccount)} to ${accountLabel(event.toAccount)}: ${failureText(event.reason)}.`,
        });
        break;
      case "agent.finished":
        // A request still waiting ends with the run, as when plxd stopped without resolving it.
        items.forEach((item, i) => {
          if (item.kind === "approval" && !item.resolved)
            items[i] = { ...item, resolved: { decision: "withdrawn", by: "stop", at: time } };
        });
        push({ kind: "end", key: key(), outcome: event.outcome });
        break;
      case "agent.wakeupsPaused":
        push({
          kind: "notice",
          key: key(),
          tone: "info",
          text: "Wake-ups are paused: finished subagents won't wake the coordinator. Your next message resumes them.",
        });
        break;
      case "agent.output":
        event.items.forEach((item, i) => applyOutput(items, item, key(i), time));
        break;
    }
    for (let i = before; i < items.length; i++) items[i] = { ...items[i]!, at: time };
  }
  return { run, items, seq };
}

/** A run as `event` leaves it: `agent.started` sets it, and `agent.updated` and fallbacks change it. */
export function updateRun(run: AgentRun | undefined, event: ParallaxEvent): AgentRun | undefined {
  switch (event.kind) {
    case "agent.started":
      return event.run;
    case "agent.updated":
      // `error` is absent once the run goes again; the rest only ever arrives.
      return run && { ...run, ...event.state, error: event.state.error };
    case "agent.accountFallback":
      return run && { ...run, accountId: event.toAccount };
    default:
      return run;
  }
}

// ponytail: copies the item list per event and scans back for matches; fine for
// thousands of items, since plxd coalesces output every 50 ms.
function applyOutput(items: Item[], item: AgentOutputItem, key: string, time: string) {
  const last = items.at(-1);
  // The assistant message a text item continues: same vendor id, or the partial one just before.
  const target = (messageId?: string) => {
    const i = messageId
      ? items.findLastIndex((x) => x.kind === "assistant" && x.messageId === messageId)
      : last?.kind === "assistant" && last.partial
        ? items.length - 1
        : -1;
    return i < 0 ? undefined : { i, item: items[i] as Extract<Item, { kind: "assistant" }> };
  };

  switch (item.kind) {
    case "sessionStarted":
      items.push({ kind: "session", key, sessionId: item.sessionId });
      break;
    case "turnStarted": {
      const images = item.images?.length ? { images: item.images } : {};
      if (item.turnId)
        items.push({
          kind: "user",
          key,
          text: item.text ?? null,
          turnId: item.turnId,
          ...(item.wake && { wake: true }),
          ...(item.from && { from: item.from }),
          ...images,
        });
      else {
        // The run's first turn has no id; its prompt came with agent.started, and gets its images.
        const i = items.findIndex((x) => x.kind === "user" && !x.turnId);
        const prompt = items[i];
        if (prompt?.kind === "user" && item.images?.length)
          items[i] = { ...prompt, images: item.images };
      }
      break;
    }
    case "textDelta": {
      const found = target(item.messageId);
      if (found) items[found.i] = { ...found.item, text: found.item.text + item.text };
      else
        items.push({
          kind: "assistant",
          key,
          text: item.text,
          messageId: item.messageId,
          partial: true,
        });
      break;
    }
    case "text": {
      const found = target(item.messageId);
      const message = { kind: "assistant", text: item.text, messageId: item.messageId } as const;
      if (found) items[found.i] = { ...message, key: found.item.key, at: found.item.at };
      else items.push({ ...message, key });
      break;
    }
    case "reasoning":
      items.push({ kind: "reasoning", key, text: item.text });
      break;
    case "toolCall": {
      const { callId, name, input } = item;
      const runId = name?.startsWith(plxdTools)
        ? (input as { runId?: unknown } | null | undefined)?.runId
        : undefined;
      const subagent = typeof runId === "string" ? subagentTitle(items, runId) : undefined;
      items.push({ kind: "tool", key, callId, name, input, ...(subagent && { subagent }) });
      break;
    }
    case "toolResult": {
      const i = items.findLastIndex((x) => x.kind === "tool" && x.callId === item.callId);
      const result = { status: item.status, output: item.output };
      if (i >= 0) items[i] = { ...(items[i] as Extract<Item, { kind: "tool" }>), ...result };
      else items.push({ kind: "tool", key, callId: item.callId, name: null, ...result });
      break;
    }
    case "todoList":
      items.push({ kind: "todo", key, items: item.items });
      break;
    case "notice":
      items.push({ kind: "notice", key, tone: "info", text: item.detail });
      break;
    case "warning":
      items.push({ kind: "notice", key, tone: "warning", text: item.detail });
      break;
    case "followUpDropped":
      items.push({
        kind: "notice",
        key,
        tone: "warning",
        text: "A message didn't reach the agent because it stopped first.",
        turnId: item.turnId,
      });
      break;
    case "interrupted":
      items.push({
        kind: "notice",
        key,
        tone: "info",
        text: "Stopped by another thread.",
        from: item.from,
      });
      break;
    case "turnFinished": {
      // The turn's final text, when it isn't already the last thing the agent said.
      const said = items.findLast((x) => x.kind === "assistant")?.text.trim();
      if (item.result?.trim() && item.result.trim() !== said)
        items.push({ kind: "assistant", key, text: item.result });
      break;
    }
    case "approvalRequested": {
      const { kind: _, ...request } = item;
      items.push({ kind: "approval", key, request });
      // Claude Code fills ExitPlanMode's request from the plan file the model wrote, and the call
      // itself may carry no plan. The call takes the request's, so its plan card shows it (0031).
      const plan = isObject(item.input) ? item.input["plan"] : undefined;
      const i = items.findLastIndex((x) => x.kind === "tool" && x.callId === item.callId);
      const call = items[i];
      if (
        item.toolName === "ExitPlanMode" &&
        typeof plan === "string" &&
        call?.kind === "tool" &&
        call.name === "ExitPlanMode" &&
        (call.input === undefined || isObject(call.input)) &&
        typeof call.input?.["plan"] !== "string"
      )
        items[i] = { ...call, input: { ...call.input, plan } };
      break;
    }
    case "approvalResolved": {
      const { kind: _, approvalId, ...resolution } = item;
      const i = items.findLastIndex(
        (x) => x.kind === "approval" && x.request.approvalId === approvalId,
      );
      const asked = items[i];
      if (asked?.kind === "approval")
        items[i] = { ...asked, resolved: { ...resolution, at: time } };
      break;
    }
    // usage isn't shown.
  }
}

const isObject = (v?: JsonValue): v is Record<string, JsonValue> =>
  !!v && typeof v === "object" && !Array.isArray(v);

/** The permission requests still waiting, oldest first. */
export const waitingApprovals = (items: readonly Item[]): Approval[] =>
  items.filter((i): i is Approval => i.kind === "approval" && !i.resolved);

/** By run id: a run's permission requests, kept while they wait (`trackApprovals`). */
export type ApprovalsByRun = Readonly<Record<string, Transcript>>;

/**
 * Applies a Project's events to each run's waiting permission requests: a request joins, and its
 * resolution or its run's next `agent.finished` takes it out, as `applyEvents` reads them. Repeats
 * are skipped by each run's `seq`, so pages of a run's log and the Project's subscription can
 * overlap.
 */
export function trackApprovals(
  byRun: ApprovalsByRun,
  events: readonly LoggedEvent[],
): ApprovalsByRun {
  let next = byRun;
  for (const logged of events) {
    const { event } = logged;
    let kept: Extract<ParallaxEvent, { kind: "agent.output" | "agent.finished" }>;
    if (event.kind === "agent.output") {
      const items = event.items.filter(
        (i) => i.kind === "approvalRequested" || i.kind === "approvalResolved",
      );
      if (items.length === 0) continue;
      kept = { ...event, items };
    } else if (event.kind === "agent.finished") kept = event;
    else continue;
    const t = applyEvents(
      next[kept.runId] ?? emptyTranscript,
      [{ ...logged, event: kept }],
      kept.runId,
    );
    next = { ...next, [kept.runId]: { seq: t.seq, items: waitingApprovals(t.items) } };
  }
  return next;
}

/** The prefix of a coordinator's plxd tools as Claude Code names them (0019), `mcp__plxd__spawn_agent`. */
export const plxdTools = "mcp__plxd__";

/**
 * The first line of subagent `runId`'s prompt, from the newest earlier plxd tool answer that
 * lists it: spawn_agent's, message_agent's, or cancel_agent's run, agent_status's `run`, or
 * list_agents' `runs`.
 */
function subagentTitle(items: Item[], runId: string): string | undefined {
  type Summary = { runId?: unknown; prompt?: unknown };
  for (const x of items.toReversed()) {
    if (x.kind !== "tool" || !x.name?.startsWith(plxdTools) || !x.output?.includes(runId)) continue;
    try {
      const answer = JSON.parse(x.output) as Summary & { run?: Summary; runs?: Summary[] };
      const run = [answer, answer.run, ...(answer.runs ?? [])].find((r) => r?.runId === runId);
      if (typeof run?.prompt === "string") return run.prompt.trim().split("\n")[0];
    } catch {
      // Not JSON, or cut short: an older answer may still have it.
    }
  }
  return undefined;
}

/** A run of agent activity between its messages, collapsed to one row: thinking, tool calls, and checklists. */
export interface Work {
  kind: "work";
  key: string;
  items: Item[];
  /** When the work began, and when what followed it (the answer, or the end) did. */
  startedAt?: string;
  endedAt?: string;
}

/**
 * The rows of each turn an `end` row closes that fold into its work, as T3 Code shows a finished
 * turn (PLX-326): its messages before the last, and its answered permission requests. One CLI
 * process can run several turns, as follow-ups arrive, so its `end` closes them all. A turn still
 * going has no `end`, so its messages stream in place and its requests stay in view.
 */
function finishedTurnRows(rows: readonly { kind: string }[]): Set<number> {
  const folds = new Set<number>();
  // The current turn's messages and answered requests, and the earlier turns' that would fold.
  let turn: number[] = [];
  let closed: number[] = [];
  const close = () => {
    const answer = turn.findLast((j) => rows[j]!.kind === "assistant");
    closed.push(...turn.filter((j) => j !== answer));
    turn = [];
  };
  rows.forEach((row, i) => {
    if (row.kind === "user" || row.kind === "pending") close();
    else if (row.kind === "assistant" || (row.kind === "approval" && (row as Approval).resolved))
      turn.push(i);
    else if (row.kind === "end") {
      close();
      for (const j of closed) folds.add(j);
      closed = [];
    }
  });
  return folds;
}

/**
 * Folds each run of thinking, tool calls, and checklists into one `Work` row. While a turn goes,
 * the agent's messages split runs and pass through, so they stay in order and stream in place;
 * once it ends, all but its last fold too (`finishedTurnRows`). A dropped follow-up's notice,
 * another thread's stop, and other rows (user, end, and whatever the caller adds) pass through. Other notices fold, except
 * those after a run's last activity.
 */
export function groupWork<R extends { kind: string; at?: string }>(
  rows: readonly (Item | R)[],
): (Item | R | Work)[] {
  const folds = finishedTurnRows(rows);
  const out: (Item | R | Work)[] = [];
  let run: Item[] = [];
  const flush = (next?: { kind: string; at?: string }) => {
    const last = run.findLastIndex((i) => i.kind !== "notice");
    if (last >= 0) {
      const items = run.slice(0, last + 1);
      out.push({
        kind: "work",
        key: `work:${items[0]!.key}`,
        items,
        startedAt: items[0]!.at,
        // A later follow-up's time would count the wait between turns as work.
        endedAt:
          (run[last + 1] ?? (next?.kind === "assistant" || next?.kind === "end" ? next : undefined))
            ?.at ?? items[last]!.at,
      });
    }
    out.push(...run.slice(last + 1));
    run = [];
  };
  for (const [i, row] of rows.entries()) {
    if (
      folds.has(i) ||
      ["reasoning", "tool", "todo"].includes(row.kind) ||
      (row.kind === "notice" && !(row as Item & { turnId?: string }).turnId && !("from" in row))
    )
      run.push(row as Item);
    else {
      flush(row);
      out.push(row);
    }
  }
  flush();
  return out;
}

/** "Worked for 1m 29s", or "Worked briefly" when the times are missing or under a second. */
export function workedFor(startedAt?: string, endedAt?: string): string {
  const s = Math.round((Date.parse(endedAt ?? "") - Date.parse(startedAt ?? "")) / 1000);
  if (!(s >= 1)) return "Worked briefly";
  const [h, m] = [Math.floor(s / 3600), Math.floor((s % 3600) / 60)];
  const parts = h ? [`${h}h`, `${m}m`] : m ? [`${m}m`, `${s % 60}s`] : [`${s}s`];
  return `Worked for ${parts.join(" ")}`;
}

/** An account for people: a subscription is named by its backend, a key account by its id. */
export function accountLabel(accountId: string): string {
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-/i.test(accountId)) return "API key";
  return `${accountId.charAt(0).toUpperCase()}${accountId.slice(1)} subscription`;
}

const failures: Record<AgentFailureKind, string> = {
  notSignedIn: "not signed in",
  rateLimited: "rate limited",
  policyViolation: "stopped by Parallax's safety check",
  unexpectedApiKey: "found an unexpected API key",
  vendorError: "the provider returned an error",
  crashed: "the CLI crashed",
  spawnFailed: "the CLI didn't start",
  commitFailed: "Parallax couldn't commit its changes",
  internal: "something went wrong in plxd",
};

/** Why a run failed or moved accounts, for people. */
export const failureText = (kind: AgentFailureKind) =>
  (failures as Partial<Record<string, string>>)[kind] ?? "something went wrong";

const statuses: Record<AgentStatus, string> = {
  starting: "Starting",
  running: "Working",
  completed: "Done",
  failed: "Failed",
  cancelled: "Stopped",
  interrupted: "Interrupted",
  waiting: "Waiting",
  accepted: "Accepted",
};

/** A run's status for people. A status newer than this app is "Unknown". */
export const statusLabel = (status: AgentStatus) =>
  (statuses as Partial<Record<string, string>>)[status] ?? "Unknown";

/** Whether the run's CLI is live, so Stop applies. */
export const isRunning = (status?: AgentStatus) => status === "starting" || status === "running";
