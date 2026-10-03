import { expect, test } from "vite-plus/test";

import samples from "../../../../crates/parallax-protocol/samples/v1/agents.json";
import type { AgentOutputItem, LoggedEvent, ParallaxEvent } from "../protocol/generated/protocol";
import {
  applyEvents,
  emptyTranscript,
  groupWork,
  trackApprovals,
  waitingApprovals,
  workedFor,
  type Item,
  type Work,
} from "./transcript";
import { uuidv7 } from "./uuidv7";

const runId = "01a0d360-1a2b-7c3d-8e4f-5a6b7c8d9e01";
// agents.json's events, in log order: a run's whole life, then a fallback and three endings.
const logged = (samples as { method?: string; params?: unknown }[])
  .filter((m) => m.method === "events/event")
  .map((m) => m.params as LoggedEvent);
const upTo = (seq: number) => logged.filter((e) => e.seq <= seq);

let nextSeq = 100;
const at = (event: ParallaxEvent): LoggedEvent => ({ seq: nextSeq++, time: "", event });
const output = (...items: AgentOutputItem[]) => at({ kind: "agent.output", runId, items });
const build = (...events: LoggedEvent[]) => applyEvents(emptyTranscript, events, runId);
const of = <K extends Item["kind"]>(items: Item[], kind: K) =>
  items.filter((i): i is Extract<Item, { kind: K }> => i.kind === kind);

test("rebuilds the sample run's transcript, item by item", () => {
  const t = build(...upTo(8));
  expect(t.seq).toBe(8);
  expect(t.items.map((i) => i.kind)).toEqual([
    "user", // the prompt, from agent.started
    "session", // the CLI's session, never shown (RYA-250)
    "assistant", // msg_1: its delta, then its full text
    "reasoning",
    "todo",
    "tool", // Write: ok
    "tool", // Bash: input too large, denied
    "tool", // a result with no call
    "notice",
    "notice", // the warning
    "assistant", // the turn's result, which it hadn't said yet
    "user", // the follow-up, by turn id only
    "assistant", // its result
    "notice", // the follow-up was dropped
    "end",
  ]);

  const [prompt, followUp] = of(t.items, "user");
  expect(prompt).toMatchObject({ text: "Add a README that explains how to build the app." });
  expect(followUp).toMatchObject({ text: null, turnId: "01a0d361-2b3c-7d4e-9f50-6a7b8c9d0e11" });
  expect(of(t.items, "assistant").map((i) => i.text)).toEqual([
    "I'll add a README and note the build steps in shared context.",
    "Added README.md.",
    "Mentioned the tests.",
  ]);
  expect(of(t.items, "tool")).toMatchObject([
    { name: "Write", status: "ok", output: "File created" },
    { name: "Bash", status: "denied", input: { truncated: true } },
    { name: null, callId: "toolu_3", status: "error" },
  ]);
  expect(of(t.items, "session")).toEqual([
    { kind: "session", key: "3:0", sessionId: "session-7f3a", at: "2026-09-25T12:00:02Z" },
  ]);
  expect(of(t.items, "notice").map((i) => i.tone)).toEqual(["info", "warning", "warning"]);
  expect(of(t.items, "end")[0]!.outcome).toEqual({
    status: "completed",
    result: "Mentioned the tests.",
  });

  // agent.updated kept the run current; the branch came with agent.started.
  expect(t.run).toMatchObject({ status: "completed", sessionId: "session-7f3a" });
  expect(t.run!.diff).toMatchObject({ files: 2 });
});

test("a page and a live event with the same seq apply once, and other runs are skipped", () => {
  const once = build(...upTo(8));
  expect(applyEvents(once, upTo(8), runId)).toEqual(once);

  const other = { ...logged[0]!, seq: 50, event: { ...logged[0]!.event, runId: "someone-else" } };
  const after = applyEvents(once, [other as LoggedEvent], runId);
  expect(after.items).toEqual(once.items);
  expect(after.seq).toBe(50);
});

test("kinds this version doesn't know are skipped, but still count their seq", () => {
  const known = build(...upTo(1));
  const newer = [
    at({ kind: "agent.somethingNew", runId } as unknown as ParallaxEvent),
    output({ kind: "somethingNew" } as unknown as AgentOutputItem),
  ];
  const t = applyEvents(known, newer, runId);
  expect(t.items).toEqual(known.items);
  expect(t.seq).toBe(newer[1]!.seq);
});

test("an account fallback moves the run and says why", () => {
  const t = build(...upTo(9));
  expect(t.run!.accountId).toBe("01a0d34b-3c4d-7e5f-a061-7b8c9d0e1f22");
  expect(t.items.at(-1)).toMatchObject({
    kind: "notice",
    text: "Switched from Claude subscription to API key: rate limited.",
  });
});

test("each way a run ends is an end item, and an update clears a stale error", () => {
  const t = build(...logged);
  expect(of(t.items, "end").map((i) => i.outcome.status)).toEqual([
    "completed",
    "cancelled",
    "failed",
    "interrupted",
  ]);
  // seq 14 failed with an error; seq 15 no longer has one.
  expect(build(...upTo(14)).run!.error).toBe("the CLI exited with code 1");
  expect(t.run).toMatchObject({ status: "cancelled", error: undefined });
});

test("deltas without a message id stream into one message that the full text replaces", () => {
  const t = build(
    ...upTo(1),
    output({ kind: "textDelta", text: "Hel" }),
    output({ kind: "textDelta", text: "lo" }),
  );
  const streaming = t.items.at(-1)!;
  expect(streaming).toMatchObject({ kind: "assistant", text: "Hello", partial: true });

  const done = applyEvents(t, [output({ kind: "text", text: "Hello!" })], runId);
  expect(done.items).toHaveLength(2);
  expect(done.items.at(-1)).toEqual({
    kind: "assistant",
    key: streaming.key,
    text: "Hello!",
    at: "",
  });
});

test("a turn's result that repeats its last message isn't shown twice", () => {
  const t = build(
    ...upTo(1),
    output({ kind: "text", text: "All done." }, { kind: "turnFinished", result: "All done." }),
  );
  expect(of(t.items, "assistant")).toHaveLength(1);
});

test("a follow-up shows the text its turnStarted logged", () => {
  const turnId = uuidv7();
  const t = build(...upTo(1), output({ kind: "turnStarted", turnId, text: "And the tests." }));
  expect(of(t.items, "user").at(-1)).toMatchObject({ text: "And the tests.", turnId });
});

test("a message's image ids come from its turnStarted: the prompt's from the turn with no id (RYA-193)", () => {
  const turnId = uuidv7();
  const t = build(
    ...upTo(1),
    output({ kind: "turnStarted", images: ["i-1", "i-2"] }),
    output({ kind: "turnStarted", turnId, text: "", images: ["i-3"] }),
  );
  const [prompt, followUp] = of(t.items, "user");
  expect(prompt).toMatchObject({ kind: "user", images: ["i-1", "i-2"] });
  expect(prompt).not.toHaveProperty("turnId");
  expect(followUp).toMatchObject({ text: "", turnId, images: ["i-3"] });
});

test("a wake-up is marked as Parallax's, and a pause says the next message resumes them (0025)", () => {
  const turnId = uuidv7();
  const text = "Parallax, not the user: runs you started finished.";
  const t = build(
    ...upTo(1),
    output({ kind: "turnStarted", turnId, text, wake: true }),
    at({ kind: "agent.wakeupsPaused", runId }),
  );
  expect(of(t.items, "user").at(-1)).toMatchObject({ text, turnId, wake: true });
  expect(of(t.items, "user")[0]).not.toHaveProperty("wake");
  expect(t.items.at(-1)).toMatchObject({
    kind: "notice",
    text: "Wake-ups are paused: finished subagents won't wake the coordinator. Your next message resumes them.",
  });
});

test("another thread's message and stop are marked with its run id (0041)", () => {
  const [turnId, from] = [uuidv7(), uuidv7()];
  const t = build(
    ...upTo(1),
    output({ kind: "turnStarted", turnId, text: "Rebase first.", from }),
    output({ kind: "interrupted", from }),
  );
  expect(of(t.items, "user").at(-1)).toMatchObject({ text: "Rebase first.", turnId, from });
  expect(of(t.items, "user")[0]).not.toHaveProperty("from");
  expect(t.items.at(-1)).toMatchObject({
    kind: "notice",
    text: "Stopped by another thread.",
    from,
  });
});

test("uuidv7 puts the time first and sets the version and variant", () => {
  const id = uuidv7(0x0190_1234_5678);
  expect(id).toMatch(/^01901234-5678-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(uuidv7()).not.toBe(uuidv7());
});

test("a turn's activity folds into one work row, leaving the closing answer out", () => {
  const item = (kind: "reasoning" | "tool" | "assistant", n: number, time: string) =>
    ({ kind, key: `k${n}`, text: "", callId: "", name: null, at: time }) as Item;
  const rows = [
    { kind: "user", key: "u", text: "go" } as Item,
    item("reasoning", 1, "2026-01-01T00:00:00Z"),
    item("tool", 2, "2026-01-01T00:00:20Z"),
    item("assistant", 3, "2026-01-01T00:01:29Z"),
  ];
  const grouped = groupWork(rows);
  expect(grouped.map((r) => r.kind)).toEqual(["user", "work", "assistant"]);
  expect(grouped[1]).toMatchObject({ startedAt: rows[1]!.at, endedAt: rows[3]!.at });
  // Narration alone isn't work.
  expect(groupWork([rows[3]!]).map((r) => r.kind)).toEqual(["assistant"]);

  expect(workedFor("2026-01-01T00:00:00Z", "2026-01-01T00:01:29Z")).toBe("Worked for 1m 29s");
  expect(workedFor("2026-01-01T00:00:00Z", "2026-01-01T00:00:12Z")).toBe("Worked for 12s");
  expect(workedFor("", "")).toBe("Worked briefly");
});

test("messages split work rows and stay in order; a mid-run notice folds, and a follow-up's time isn't work", () => {
  const at = (n: number) => `2026-01-01T00:00:${String(n).padStart(2, "0")}Z`;
  const rows = [
    { kind: "tool", key: "t1", callId: "1", name: "Bash", at: at(0) },
    { kind: "assistant", key: "a1", text: "Checking.", at: at(5) },
    { kind: "notice", key: "n", tone: "warning", text: "Retrying", at: at(6) },
    { kind: "tool", key: "t2", callId: "2", name: "Bash", at: at(10) },
    { kind: "assistant", key: "a2", text: "Done.", at: at(20) },
    { kind: "notice", key: "n2", tone: "info", text: "Switched", at: at(21) },
    { kind: "user", key: "u", text: "more", at: at(600) },
  ] as Item[];
  const grouped = groupWork(rows);
  expect(grouped.map((r) => r.key)).toEqual(["work:t1", "a1", "work:n", "a2", "n2", "u"]);
  expect(grouped[0]).toMatchObject({ startedAt: at(0), endedAt: at(5) });
  expect(grouped[2]).toMatchObject({ startedAt: at(6), endedAt: at(20) });

  // A turn that ends on a tool ends with its last item, not the next turn's message.
  expect(groupWork([rows[3]!, rows[6]!])[0]).toMatchObject({ endedAt: at(10) });
});

test("a finished turn folds its interim messages and answered requests, leaving its last message", () => {
  const at = (n: number) => `2026-01-01T00:00:${String(n).padStart(2, "0")}Z`;
  const turn = [
    { kind: "user", key: "u", text: "go", at: at(0) },
    { kind: "assistant", key: "a1", text: "Picking a change.", at: at(1) },
    { kind: "approval", key: "p", request: { toolName: "Bash" }, resolved: {}, at: at(2) },
    { kind: "tool", key: "t", callId: "1", name: "Bash", at: at(3) },
    { kind: "assistant", key: "a2", text: "Opened the PR.", at: at(9) },
  ] as Item[];
  const end = { kind: "end", key: "e", outcome: { status: "completed" }, at: at(10) } as Item;

  const done = groupWork([...turn, end]);
  expect(done.map((r) => r.key)).toEqual(["u", "work:a1", "a2", "e"]);
  expect(done[1]).toMatchObject({ startedAt: at(1), endedAt: at(9) });
  expect((done[1] as Work).items.map((i) => i.key)).toEqual(["a1", "p", "t"]);

  // Still going: messages and requests split the work, as before.
  expect(groupWork(turn).map((r) => r.key)).toEqual(["u", "a1", "p", "work:t", "a2"]);

  // A follow-up runs in the same process, whose one end closes both turns.
  const followUp = [
    { kind: "user", key: "u2", text: "more", at: at(11) },
    { kind: "assistant", key: "b1", text: "On it.", at: at(12) },
    { kind: "assistant", key: "b2", text: "Done.", at: at(13) },
  ] as Item[];
  expect(groupWork([...turn, ...followUp, end]).map((r) => r.key)).toEqual([
    "u",
    "work:a1",
    "a2",
    "u2",
    "work:b1",
    "b2",
    "e",
  ]);
});

test("a coordinator's plxd tool that names a subagent gets its prompt's first line from an earlier answer", () => {
  const call = (callId: string, tool: string, input: Record<string, string>) => ({
    kind: "toolCall" as const,
    callId,
    name: `mcp__plxd__${tool}`,
    input,
  });
  const answer = (callId: string, value: unknown) => ({
    kind: "toolResult" as const,
    callId,
    status: "ok" as const,
    output: JSON.stringify(value, null, 2),
  });
  const t = build(
    output(
      call("1", "spawn_agent", { prompt: "Fix the login bug\nwith a test" }),
      answer("1", { runId: "r-1", prompt: "Fix the login bug\nwith a test", status: "starting" }),
      call("2", "list_agents", {}),
      answer("2", { runs: [{ runId: "r-0", prompt: "Write the plan" }] }),
      call("3", "agent_status", { runId: "r-1" }),
      call("4", "cancel_agent", { runId: "r-0" }),
      call("5", "agent_diff", { runId: "r-9" }),
    ),
  );
  expect(of(t.items, "tool").map((i) => i.subagent)).toEqual([
    undefined,
    undefined,
    "Fix the login bug",
    "Write the plan",
    undefined, // no answer named it
  ]);
});

// RYA-196: permission requests (0031).
const asked = (approvalId: string, more: Partial<AgentOutputItem> = {}): AgentOutputItem =>
  ({
    kind: "approvalRequested",
    approvalId,
    toolName: "Bash",
    input: { command: "pnpm test" },
    callId: `toolu_${approvalId}`,
    expiresAt: "2026-10-01T12:30:00Z",
    ...more,
  }) as AgentOutputItem;
const timed = (event: ParallaxEvent, time: string): LoggedEvent => ({ ...at(event), time });

test("a permission request is an item until its resolution says how it ended, and when", () => {
  const t = build(
    timed(
      { kind: "agent.output", runId, items: [asked("a1", { alwaysAllow: ["Bash(pnpm test:*)"] })] },
      "2026-10-01T12:00:00Z",
    ),
    timed({ kind: "agent.output", runId, items: [asked("a2")] }, "2026-10-01T12:00:01Z"),
    timed(
      {
        kind: "agent.output",
        runId,
        items: [
          {
            kind: "approvalResolved",
            approvalId: "a1",
            decision: "allowed",
            by: "user",
            always: true,
          },
        ],
      },
      "2026-10-01T12:00:05Z",
    ),
  );
  const [first, second] = of(t.items, "approval");
  expect(first).toMatchObject({
    at: "2026-10-01T12:00:00Z",
    request: { approvalId: "a1", toolName: "Bash", alwaysAllow: ["Bash(pnpm test:*)"] },
    resolved: { decision: "allowed", by: "user", always: true, at: "2026-10-01T12:00:05Z" },
  });
  expect(first!.request).not.toHaveProperty("kind");
  expect(second!.resolved).toBeUndefined();
  expect(waitingApprovals(t.items).map((a) => a.request.approvalId)).toEqual(["a2"]);
});

test("the run's end ends a request still waiting, as withdrawn; one answered keeps its answer", () => {
  const t = build(
    output(asked("a1"), asked("a2")),
    output({
      kind: "approvalResolved",
      approvalId: "a1",
      decision: "denied",
      by: "user",
      message: "No.",
    }),
    {
      ...at({ kind: "agent.finished", runId, outcome: { status: "interrupted" } }),
      time: "2026-10-01T12:10:00Z",
    },
  );
  expect(of(t.items, "approval").map((a) => a.resolved)).toEqual([
    { decision: "denied", by: "user", message: "No.", at: "" },
    { decision: "withdrawn", by: "stop", at: "2026-10-01T12:10:00Z" },
  ]);
  expect(waitingApprovals(t.items)).toEqual([]);
});

test("ExitPlanMode's call takes the plan its request carries, and never loses its own", () => {
  const plan = { plan: "1. Add a README.", planFilePath: "/home/me/.claude/plans/readme.md" };
  const call = (callId: string, input: Record<string, string>) =>
    ({ kind: "toolCall", callId, name: "ExitPlanMode", input }) as const;
  const t = build(
    output(
      call("toolu_p1", {}),
      asked("p1", { toolName: "ExitPlanMode", input: plan, callId: "toolu_p1", interactive: true }),
    ),
    output(
      call("toolu_p2", { plan: "The model's own." }),
      asked("p2", { toolName: "ExitPlanMode", input: plan, callId: "toolu_p2" }),
    ),
    // With no plan in the request, the call stays as it was.
    output(
      call("toolu_p3", {}),
      asked("p3", { toolName: "ExitPlanMode", input: {}, callId: "toolu_p3" }),
    ),
  );
  expect(of(t.items, "tool").map((i) => i.input)).toEqual([
    { plan: "1. Add a README." },
    { plan: "The model's own." },
    {},
  ]);
});

test("a Project's waiting requests are tracked by run, once each, until resolved or ended", () => {
  const other = "01a0d391-0000-7000-8000-000000000001";
  const req = (seq: number, run: string, approvalId: string): LoggedEvent => ({
    seq,
    time: `2026-10-01T12:00:0${seq % 10}Z`,
    event: { kind: "agent.output", runId: run, items: [asked(approvalId)] },
  });
  // A page of the run's log, then the subscription repeating its last event.
  let byRun = trackApprovals({}, [req(1, runId, "a1"), req(2, other, "b1"), req(3, other, "b2")]);
  byRun = trackApprovals(byRun, [req(3, other, "b2")]);
  const ids = (run: string) =>
    byRun[run]!.items.map((i) => (i as { request: { approvalId: string } }).request.approvalId);
  expect(ids(runId)).toEqual(["a1"]);
  expect(ids(other)).toEqual(["b1", "b2"]);

  byRun = trackApprovals(byRun, [
    // Text and other runs' items don't matter; a resolution and the run's end do.
    {
      seq: 4,
      time: "",
      event: { kind: "agent.output", runId, items: [{ kind: "text", text: "Hi" }] },
    },
    {
      seq: 5,
      time: "",
      event: {
        kind: "agent.output",
        runId: other,
        items: [{ kind: "approvalResolved", approvalId: "b1", decision: "expired", by: "timeout" }],
      },
    },
    {
      seq: 6,
      time: "",
      event: { kind: "agent.finished", runId, outcome: { status: "cancelled" } },
    },
  ]);
  expect(ids(runId)).toEqual([]);
  expect(ids(other)).toEqual(["b2"]);
});
