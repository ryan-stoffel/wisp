// @vitest-environment happy-dom
import type { TiptapEditorHTMLElement } from "@tiptap/react";
import { Globe } from "lucide-react";
import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vite-plus/test";

import samples from "../../../../crates/parallax-protocol/samples/v1/agents.json";
import type { ConnectionState, SubscriptionMessage, ParallaxBridge } from "../preload/bridge";
import type {
  AgentRun,
  AgentRunResult,
  AgentTodoItem,
  AgentToolStatus,
  LoggedEvent,
} from "../protocol/generated/protocol";
import { activity, AgentChat, linkIcon, RowView, RunTab, TranscriptView } from "./AgentChat";
import { Composer } from "./Composer";
import { GitHubLogo, LinearLogo } from "./logos";
import type { Item } from "./transcript";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// happy-dom has no popovers. Menus are in the DOM either way.
HTMLElement.prototype.hidePopover = () => {};
// happy-dom lays nothing out. Give the transcript a tall viewport and each row a
// small height, so the virtualized list renders every row.
Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
  get(this: HTMLElement) {
    return this.getAttribute("role") === "log" ? 10_000 : 20;
  },
});

const runId = "01a0d360-1a2b-7c3d-8e4f-5a6b7c8d9e01";
const logged = (samples as { method?: string; params?: unknown }[])
  .filter((m) => m.method === "events/event")
  .map((m) => m.params as LoggedEvent);

let unmount = () => {};
afterEach(() => {
  act(() => unmount());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function render(node: ReactNode) {
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root.render(node));
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
    unmount = () => {};
  };
}

// The composer's editor, which Tiptap keeps on its element for tests.
const composer = () =>
  document.querySelector<TiptapEditorHTMLElement>('[role="textbox"][aria-label="Message"]')!;
const type = (text: string) => act(() => void composer().editor!.commands.setContent(text));

const row = (item: Item) =>
  render(<RowView row={item} live={false} open={false} onToggle={() => {}} />);
// A row's summary as it shows, without what only screen readers hear.
const shown = () => {
  const summary = document.querySelector("summary")!.cloneNode(true) as Element;
  summary.querySelectorAll(".sr-only").forEach((e) => e.remove());
  return summary.textContent;
};
const said = () => document.querySelector("summary .sr-only")?.textContent;

test("a message on its way to the agent shows at full strength", () => {
  render(
    <RowView
      row={{ kind: "pending", key: "p", text: "And this?" }}
      live={false}
      open={false}
      onToggle={() => {}}
    />,
  );
  expect(document.body.textContent).toBe("And this?");
  expect(document.querySelector(".bg-selected")!.className).not.toContain("opacity");
});

test("a prompt shows when it was sent and copies its text, on hover", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  row({ kind: "user", key: "a", text: "Fix the build", at: "2026-09-25T12:00:00Z" });
  const time = document.querySelector("time")!;
  expect(time.dateTime).toBe("2026-09-25T12:00:00Z");
  expect(time.parentElement!.className).toContain("group-hover/prompt:opacity-100");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Copy message"]')!.click(),
  );
  expect(writeText).toHaveBeenCalledWith("Fix the build");
  expect(document.querySelector('button[aria-label="Copied"]')).not.toBeNull();
});

test("a user message shows its text, or a neutral label when the log has none", () => {
  row({ kind: "user", key: "a", text: "Fix the build" });
  expect(document.body.textContent).toBe("Fix the build");
  act(() => unmount());
  row({ kind: "user", key: "b", text: null, turnId: "t" });
  expect(document.body.textContent).toBe("Follow-up message");
});

test("a user message shows its images over its text: fetched by id, or at hand when sent from here", async () => {
  const url = (data: string) => `data:image/png;base64,${data}`;
  const loadImage = vi.fn(async (id: string) => (id === "i-1" ? url("AAAA") : undefined));
  render(
    <RowView
      row={{ kind: "user", key: "a", text: "", images: ["i-1", "i-2"] }}
      live={false}
      open={false}
      onToggle={() => {}}
      loadImage={loadImage}
    />,
  );
  await act(async () => {});
  expect([...document.querySelectorAll("img")].map((img) => img.getAttribute("src"))).toEqual([
    url("AAAA"),
  ]);
  expect(document.querySelector('[aria-label="Image unavailable"]')).not.toBeNull();
  // Images alone get no bubble.
  expect(document.body.textContent).toBe("");
  act(() => unmount());

  render(
    <RowView
      row={{
        kind: "pending",
        key: "p",
        text: "And this?",
        images: [{ mediaType: "image/png", data: "BBBB" }],
      }}
      live={false}
      open={false}
      onToggle={() => {}}
    />,
  );
  expect(document.querySelector("img")?.getAttribute("src")).toBe(url("BBBB"));
  expect(document.body.textContent).toBe("And this?");
});

test("a wake-up reads as from Parallax, with its message folded away", () => {
  row({
    kind: "user",
    key: "w",
    text: "Parallax, not the user: runs you started finished.",
    wake: true,
  });
  expect(document.querySelector("summary")!.textContent).toBe("From Parallax: subagents finished");
  expect(document.querySelector("details")!.open).toBe(false);
  expect(document.querySelector(".bg-selected")).toBeNull();
});

test("an assistant message renders Markdown, but never raw HTML or images", () => {
  row({
    kind: "assistant",
    key: "a",
    text: "Run **this**:\n\n```sh\ncargo test\n```\n\nSee [docs](https://example.com). <img src=x onerror=alert(1)> ![a diagram](file:///etc/passwd)",
  });
  expect(document.querySelector("strong")?.textContent).toBe("this");
  expect(document.querySelector("pre code")?.textContent).toBe("cargo test\n");
  expect(document.querySelector('button[aria-label="Copy code"]')).not.toBeNull();
  const link = document.querySelector("a")!;
  expect(link.getAttribute("href")).toBe("https://example.com");
  expect(link.target).toBe("_blank");
  // A Markdown image is a link with its alt text, which main opens only if it's https.
  expect(document.querySelector("img")).toBeNull();
  const links = [...document.querySelectorAll("a")];
  expect(links.map((a) => a.textContent)).toEqual(["docs", "a diagram"]);
  // Its file: source is unsafe, so it has no href at all rather than an empty one.
  expect(links[1]!.hasAttribute("href")).toBe(false);
});

test("a code block names its language, highlights it, and copies its text", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  row({ kind: "assistant", key: "a", text: "```ts\nconst x = 1;\n```\n\n```\nplain\n```" });
  const [ts, plain] = [...document.querySelectorAll(".markdown > div")];
  expect(ts!.firstElementChild!.textContent).toBe("tsCopy");
  expect(ts!.querySelector(".hljs-keyword")?.textContent).toBe("const");
  expect(plain!.firstElementChild!.textContent).toBe("textCopy");
  expect(plain!.querySelector("[class^='hljs-']")).toBeNull();
  await act(async () => ts!.querySelector<HTMLButtonElement>("button")!.click());
  expect(writeText).toHaveBeenCalledWith("const x = 1;\n");
});

test("a diff code block shows added and removed lines, and copies the diff", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  const diff = "--- a/q.sql\n+++ b/q.sql\n@@ -1,3 +1,2 @@\n-old\n--- a note\n+new\n same\n";
  row({ kind: "assistant", key: "a", text: `\`\`\`diff\n${diff}\`\`\`` });
  // The block's header, then its lines.
  const lines = [...document.querySelector(".markdown > div")!.lastElementChild!.children];
  expect(lines.map((l) => l.textContent)).toEqual([
    "--- a/q.sql",
    "+++ b/q.sql",
    "@@ -1,3 +1,2 @@",
    "−Removed: old",
    "−Removed: -- a note",
    "+Added: new",
    " same",
  ]);
  expect(lines[5]!.className).toContain("bg-added/10");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Copy code"]')!.click(),
  );
  expect(writeText).toHaveBeenCalledWith(diff);
});

test("an edit's tool call shows its change as a diff, not JSON", () => {
  row({
    kind: "tool",
    key: "t",
    callId: "toolu_3",
    name: "Edit",
    input: { file_path: "/a.ts", old_string: "let a = 1;", new_string: "let a = 2;" },
    status: "ok",
  });
  const diff = document.querySelector('[role="group"]')!;
  expect(diff.getAttribute("aria-label")).toBe("Diff: 1 line added, 1 removed");
  expect(diff.textContent).toBe("−Removed: let a = 1;+Added: let a = 2;");
  expect(document.querySelector("details")!.textContent).not.toContain("old_string");
});

test("a tool call collapses its input and output under its name", () => {
  const toggled = vi.fn();
  const tool: Item = {
    kind: "tool",
    key: "t",
    callId: "toolu_1",
    name: "Bash",
    input: { command: "cargo test\n--quiet" },
    status: "error",
    output: "1 failed",
  };
  render(<RowView row={tool} live={false} open={false} onToggle={toggled} />);
  const details = document.querySelector("details")!;
  expect(details.open).toBe(false);
  expect(shown()).toBe("Bashcargo test");
  expect(said()).toBe("Failed");

  act(() => {
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
  });
  expect(toggled).toHaveBeenCalledWith("t", true);
  expect(details.textContent).toContain("1 failed");
});

test("a tool call with an oversized input says so", () => {
  row({
    kind: "tool",
    key: "t",
    callId: "toolu_2",
    name: "Bash",
    input: { truncated: true, bytes: 90210 },
    status: "denied",
  });
  expect(document.querySelector("details")!.textContent).toContain("Too large to show (89 KB)");
});

test("a coordinator's plxd tool calls read as what they did, and to what", () => {
  const summary = (name: string, input: Record<string, string>, subagent?: string) => {
    row({ kind: "tool", key: "t", callId: "1", name, input, ...(subagent && { subagent }) });
    const text = shown();
    act(() => unmount());
    return text;
  };
  expect(summary("mcp__plxd__spawn_agent", { prompt: "Fix the login bug\nwith a test" })).toBe(
    "Started a subagentFix the login bug",
  );
  expect(summary("mcp__plxd__agent_status", { runId: "r-1" }, "Fix the login bug")).toBe(
    "Checked on a subagentFix the login bug",
  );
  expect(summary("mcp__plxd__write_context", { path: "plan.md", content: "# Plan" })).toBe(
    "Wrote shared contextplan.md",
  );
  // A plxd tool this app doesn't know reads as any MCP server's tool does.
  expect(summary("mcp__plxd__plan_approve", {})).toBe("plxdplan approve");
});

test("another MCP server's tool reads as the server and the tool, and a skill by its name", () => {
  const summary = (name: string, input: Record<string, string>) => {
    row({ kind: "tool", key: "t", callId: "1", name, input });
    const text = shown();
    act(() => unmount());
    return text;
  };
  expect(summary("mcp__linear__save_issue", { title: "Fix it" })).toBe("Linearsave issue");
  expect(summary("mcp__claude-code-remote__list_repos", {})).toBe("Claude code remotelist repos");
  expect(summary("Skill", { skill: "code-review" })).toBe("Skillcode-review");
  // Older Claude Code versions name it `command`.
  expect(summary("Skill", { command: "simplify" })).toBe("Skillsimplify");
});

test("each kind of work has its own loader, and MCP tools and skills read by name", () => {
  const tool = (name: string | null, input: Record<string, string> = {}): Item => ({
    kind: "tool",
    key: "t",
    callId: "1",
    name,
    input,
  });
  const loader = (kind: string, variant: string) => ({ kind, variant });
  expect(activity({ kind: "reasoning", key: "r", text: "Hm" })).toEqual({
    label: "Thinking",
    loader: loader("matrix", "ripple"),
  });
  expect(activity(tool("Bash", { command: "cargo test\n--quiet" }))).toEqual({
    label: "Running",
    detail: "cargo test",
    loader: loader("register", "shift"),
  });
  expect(activity(tool("Read", { file_path: "src/main.rs" }))).toEqual({
    label: "Reading",
    detail: "src/main.rs",
    loader: loader("bands", "descend"),
  });
  expect(activity(tool("Grep", { pattern: "TODO" })).loader).toEqual(loader("matrix", "scan"));
  expect(activity(tool("WebSearch")).loader).toEqual(loader("matrix", "scan"));
  expect(activity(tool("Edit")).loader).toEqual(loader("cells", "merge"));
  expect(activity(tool("Write")).loader).toEqual(loader("cells", "merge"));
  expect(activity(tool("WebFetch")).loader).toEqual(loader("beacon", "rise"));
  expect(activity(tool("Task", { description: "Find the bug" }))).toEqual({
    label: "Running agent",
    detail: "Find the bug",
    loader: loader("orbit", "oppose"),
  });
  expect(activity(tool("Skill", { skill: "code-review" }))).toEqual({
    label: "Using skill",
    detail: "code-review",
    loader: loader("lift", "rise"),
  });
  expect(activity(tool("mcp__linear__save_issue", { title: "Fix it" }))).toEqual({
    label: "Using Linear",
    detail: "save issue",
    loader: loader("beacon", "balance"),
  });
  expect(activity(tool("mcp__plxd__spawn_agent", { prompt: "Fix the login bug" }))).toEqual({
    label: "Started a subagent",
    detail: "Fix the login bug",
    loader: loader("cells", "spread"),
  });
  // A plxd tool this app doesn't know reads as any MCP server's tool does, with plxd's loader.
  expect(activity(tool("mcp__plxd__plan_approve"))).toEqual({
    label: "Using plxd",
    detail: "plan approve",
    loader: loader("cells", "spread"),
  });
  expect(activity({ kind: "todo", key: "c", items: [] })).toEqual({
    label: "Planning",
    loader: loader("lift", "breathe"),
  });
  // Anything else.
  expect(activity(tool("Frobnicate", { path: "a.txt" }))).toEqual({
    label: "Frobnicate",
    detail: "a.txt",
    loader: loader("orbit", "chase"),
  });
  expect(activity(tool(null))).toMatchObject({
    label: "Working",
    loader: loader("orbit", "chase"),
  });
  expect(activity()).toEqual({ label: "Working", loader: loader("orbit", "chase") });
});

test("a running tool call draws its work's loader, and a finished one its kind's icon", () => {
  const tool: Item = { kind: "tool", key: "t", callId: "1", name: "Read", input: {} };
  render(<RowView row={tool} live open={false} onToggle={() => {}} />);
  const summary = () => document.querySelector("summary")!;
  expect(summary().querySelector('[data-loader="bands"][data-variant="descend"]')).not.toBeNull();
  // Screen readers hear the word, not the loader.
  expect(summary().querySelector(".loader")!.closest('[aria-hidden="true"]')).not.toBeNull();
  expect(said()).toBe("Running");
  act(() => unmount());

  render(<RowView row={{ ...tool, status: "ok" }} live open={false} onToggle={() => {}} />);
  expect(document.querySelector(".loader")).toBeNull();
  expect(summary().querySelector("svg.lucide-file-text")).not.toBeNull();
  expect(said()).toBe("Succeeded");
});

test("each kind of tool has its icon once it's done, as its loader matches it while it runs", () => {
  // The icon each finished tool shows, by its lucide name.
  const icon = (name: string | null, input: Record<string, string> = {}) => {
    row({ kind: "tool", key: "t", callId: "1", name, input, status: "ok" });
    const svg = document.querySelector("summary svg:not(.lucide-chevron-right)")!;
    act(() => unmount());
    return svg.getAttribute("class")!.split(" ")[1];
  };
  expect(
    Object.fromEntries(
      [
        "Task",
        "Agent",
        "Bash",
        "Read",
        "NotebookRead",
        "LS",
        "Grep",
        "Glob",
        "WebSearch",
        "ToolSearch",
        "Edit",
        "MultiEdit",
        "NotebookEdit",
        "Write",
        "WebFetch",
        "mcp__linear__save_issue",
        "Skill",
        "mcp__plxd__spawn_agent",
        "mcp__plxd__plan_approve",
        "TodoWrite",
        "TaskCreate",
        "TaskUpdate",
        "TaskList",
        "TaskGet",
        "Frobnicate",
      ].map((name) => [name, icon(name)]),
    ),
  ).toEqual({
    Task: "lucide-bot",
    Agent: "lucide-bot",
    Bash: "lucide-square-terminal",
    Read: "lucide-file-text",
    NotebookRead: "lucide-file-text",
    LS: "lucide-file-text",
    Grep: "lucide-search",
    Glob: "lucide-search",
    WebSearch: "lucide-search",
    ToolSearch: "lucide-search",
    Edit: "lucide-pencil",
    MultiEdit: "lucide-pencil",
    NotebookEdit: "lucide-pencil",
    Write: "lucide-file-plus",
    WebFetch: "lucide-globe",
    mcp__linear__save_issue: "lucide-plug",
    Skill: "lucide-sparkles",
    mcp__plxd__spawn_agent: "lucide-workflow",
    // A plxd tool this app doesn't know is still plxd's.
    mcp__plxd__plan_approve: "lucide-workflow",
    TodoWrite: "lucide-list-checks",
    // Claude Code's task tools plan as TodoWrite did (RYA-248).
    TaskCreate: "lucide-list-checks",
    TaskUpdate: "lucide-list-checks",
    TaskList: "lucide-list-checks",
    TaskGet: "lucide-list-checks",
    Frobnicate: "lucide-hammer",
  });
  expect(icon(null)).toBe("lucide-hammer");

  // Thinking has the brain.
  row({ kind: "reasoning", key: "r", text: "Hm" });
  expect(document.querySelector("summary svg.lucide-brain")).not.toBeNull();
});

test("a failed, denied, or unfinished tool call marks its icon, and says how it went in words", () => {
  const tool = (status?: AgentToolStatus): Item => ({
    kind: "tool",
    key: "t",
    callId: "1",
    name: "Bash",
    input: { command: "git push" },
    ...(status && { status }),
  });
  // The kind's icon, whether it fades, and the mark at its corner.
  const slot = () => {
    const icon = document.querySelector("summary .lucide-square-terminal")!;
    return {
      color: icon.parentElement!.className.match(/text-[\w-]+/)?.[0],
      faded: icon.classList.contains("opacity-50"),
      mark: icon.nextElementSibling?.getAttribute("class")?.split(" ")[1],
      said: said(),
    };
  };
  const unfinished = {
    color: "text-faint-foreground",
    faded: true,
    mark: "lucide-ellipsis",
    said: "No result",
  };
  row(tool("error"));
  expect(slot()).toEqual({ color: "text-danger", faded: false, mark: "lucide-x", said: "Failed" });
  act(() => unmount());
  row(tool("denied"));
  expect(slot()).toEqual({
    color: "text-danger",
    faded: false,
    mark: "lucide-ban",
    said: "Denied",
  });
  act(() => unmount());
  // No result, and the run has stopped: unfinished.
  row(tool());
  expect(slot()).toEqual(unfinished);
  act(() => unmount());
  // A status from a newer plxd reads as no result, rather than breaking the row.
  row(tool("cancelled" as AgentToolStatus));
  expect(slot()).toEqual(unfinished);
  act(() => unmount());
  row(tool("ok"));
  expect(slot()).toEqual({
    color: "text-muted-foreground",
    faded: false,
    mark: undefined,
    said: "Succeeded",
  });
});

test("a tool named like an object's own property is any other tool, running or done", () => {
  const tool: Item = { kind: "tool", key: "t", callId: "1", name: "constructor", input: {} };
  expect(activity(tool).loader).toEqual({ kind: "orbit", variant: "chase" });
  render(<RowView row={tool} live open={false} onToggle={() => {}} />);
  expect(
    document.querySelector('summary [data-loader="orbit"][data-variant="chase"]'),
  ).not.toBeNull();
  expect(said()).toBe("Running");
  act(() => unmount());
  render(<RowView row={{ ...tool, status: "ok" }} live open={false} onToggle={() => {}} />);
  expect(document.querySelector("summary svg.lucide-hammer")).not.toBeNull();
  expect(said()).toBe("Succeeded");
});

test("a coordinator's no-write stop lists the files it changed", () => {
  row({
    kind: "end",
    key: "e",
    outcome: {
      status: "failed",
      failure: "policyViolation",
      message:
        "the coordinator's no-write turn changed the working tree:\n M src/settings.tsx\n?? notes.md",
    },
  });
  const alert = document.querySelector('[role="alert"]')!;
  expect(alert.querySelector("p")!.textContent).toBe("Failed: stopped by Parallax's safety check");
  expect(alert.textContent).toContain("the coordinator's no-write turn changed the working tree:");
  expect(alert.querySelector("pre")!.textContent).toBe(" M src/settings.tsx\n?? notes.md");
});

test("reasoning, plan updates, and notices render quietly", () => {
  row({ kind: "reasoning", key: "r", text: "The build uses cargo." });
  expect(shown()).toBe("Thinking");
  act(() => unmount());

  // A checklist in the work is an update to the turn's plan: one line, not the list again.
  row({
    kind: "todo",
    key: "c",
    items: [
      { text: "Write README.md", status: "inProgress" },
      { text: "Read the scripts", status: "completed" },
    ],
  });
  expect(document.body.textContent).toBe("Updated the plan");
  expect(document.querySelector("li")).toBeNull();
  act(() => unmount());

  row({ kind: "notice", key: "n", tone: "warning", text: "skipped a malformed line" });
  expect(document.body.textContent).toBe("skipped a malformed line");
});

test("a failed run shows why; other endings are a divider", () => {
  row({
    kind: "end",
    key: "e",
    outcome: { status: "failed", failure: "commitFailed", message: "no git identity" },
  });
  expect(document.querySelector('[role="alert"]')!.textContent).toBe(
    "Failed: Parallax couldn't commit its changesno git identity",
  );
  act(() => unmount());
  row({ kind: "end", key: "e", outcome: { status: "cancelled" } });
  expect(document.body.textContent).toBe("Stopped");
});

// The sample's run, as it started.
const sampleRun = (logged[0]!.event as { run: AgentRun }).run;

/**
 * A bridge serving the sample's events up to `seq`, two per page, that records calls.
 * `agent/list` answers `listSeq`, and a subscribe from before it resyncs, as plxd does
 * when it can't replay that far back. The first `resyncs` subscribes resync anyway.
 * plxd advertises `capabilities`, `agent/send` answers the run as `sent` leaves it (running by
 * default), `agent/openPr` answers `prUrl`, or fails with `prError`, and `agent/image` a tiny PNG.
 * `connect` changes the connection's state.
 */
function fakeBridge(
  seq: number,
  {
    listSeq = seq,
    resyncs = 0,
    cancelError = "",
    capabilities = {},
    sent = {} as Partial<AgentRun>,
    prUrl = "",
    prError = undefined as object | undefined,
  } = {},
) {
  let listener: (m: SubscriptionMessage) => void = () => {};
  let connection: (hostId: string, state: ConnectionState) => void = () => {};
  const request = vi.fn(async (_host: string, method: string, params: { after?: number }) => {
    if (method === "agent/list") return { result: { runs: [], seq: listSeq }, logId: "log-1" };
    if (method === "agent/cancel" && cancelError)
      return { error: { code: -32000, message: cancelError } };
    if (method === "agent/send")
      return { result: { run: { ...sampleRun, status: "running", ...sent } }, logId: "log-1" };
    if (method === "agent/openPr")
      return prError ? { error: prError } : { result: { url: prUrl }, logId: "log-1" };
    if (method === "agent/image")
      return { result: { mediaType: "image/png", data: "AAAA" }, logId: "log-1" };
    if (method !== "agent/events") return { result: {}, logId: "log-1" };
    const rest = logged.filter((e) => e.seq > params.after! && e.seq <= seq);
    return { result: { events: rest.slice(0, 2), more: rest.length > 2 }, logId: "log-1" };
  });
  const unsubscribe = vi.fn();
  const subscribe = vi.fn((_host: string, params: { after: number }, l: typeof listener) => {
    listener = l;
    if (params.after < listSeq || resyncs-- > 0) queueMicrotask(() => l({ type: "resync" }));
    return unsubscribe;
  });
  window.parallax = {
    platform: "darwin",
    connectionState: async () => ({
      status: "connected",
      plxd: "0.1.0",
      protocol: 1,
      capabilities,
    }),
    onConnectionState: (l: typeof connection) => {
      connection = l;
      return () => {};
    },
    request,
    subscribe,
  } as Partial<ParallaxBridge> as ParallaxBridge;
  return {
    request,
    subscribe,
    unsubscribe,
    emit: (m: SubscriptionMessage) => act(() => listener(m)),
    connect: (state: ConnectionState) => act(() => connection("local", state)),
  };
}

const settle = async () => {
  for (let i = 0; i < 20; i++) await act(async () => {});
};

async function renderChat(noRepo?: boolean) {
  render(<AgentChat hostId="local" runId={runId} noRepo={noRepo} />);
  await settle(); // the connection state and the pages
}

const transcriptText = () => document.querySelector('[role="log"]')?.textContent ?? "";

test("loads every page, subscribes after the last seq, and appends live events", async () => {
  const { request, subscribe, unsubscribe, emit } = fakeBridge(2);
  await renderChat();
  expect(request).toHaveBeenCalledWith("local", "agent/events", { runId, after: 0 });
  expect(subscribe).toHaveBeenCalledWith(
    "local",
    { after: 2, project: "01a0d349-6e00-7c9e-80e2-0426486a8cae", logId: "log-1" },
    expect.any(Function),
  );
  expect(transcriptText()).toContain("Add a README");
  expect(document.body.textContent).toContain("Worktree"); // the footer's tab

  emit({ type: "event", event: { subscription: "s", ...logged[2]! } });
  // The agent's messages are never folded into a work dropdown.
  expect(transcriptText()).toContain("I'll add a README and note the build steps");

  // A resync reloads from the start.
  request.mockClear();
  emit({ type: "resync" });
  await settle();
  expect(request).toHaveBeenCalledWith("local", "agent/events", { runId, after: 0 });

  act(() => unmount());
  expect(unsubscribe).toHaveBeenCalled();
});

test("subscribes after the scope's snapshot seq, so repeated resyncs end", async () => {
  // The run's last event is seq 8, but its project's log is at 1000.
  const { subscribe } = fakeBridge(8, { listSeq: 1000, resyncs: 2 });
  await renderChat();
  await settle();
  expect(subscribe.mock.calls.map(([, params]) => params.after)).toEqual([1000, 1000, 1000]);
  expect(transcriptText()).toContain("Add a README");
});

test("the composer tab shows the worktree and its branch", () => {
  const started = samples.find((m) => "result" in m && m.id === 2)!;
  render(<RunTab run={(started as unknown as { result: AgentRunResult }).result.run} />);
  expect(document.body.textContent).toBe("Worktreeparallax/1a2b3c4d");
});

test("the composer tab shows a thread in the current checkout, which has no branch of its own", () => {
  const started = samples.find((m) => "result" in m && m.id === 2)!;
  const {
    branch: _,
    worktreePath: __,
    ...run
  } = (started as unknown as { result: AgentRunResult }).result.run;
  render(<RunTab run={{ ...run, checkout: true }} />);
  expect(document.body.textContent).toBe("Current checkout");
});

test("Enter sends with a fresh v7 turn id, but not while an IME is composing", async () => {
  const { request } = fakeBridge(4);
  await renderChat();
  const box = composer();
  type("Also mention the tests.");
  expect(document.querySelector('button[aria-label="Stop"]')).toBeNull();

  const dispatch = (event: Event) => act(async () => void box.dispatchEvent(event));
  const enter = (isComposing: boolean) =>
    dispatch(new KeyboardEvent("keydown", { key: "Enter", isComposing, bubbles: true }));
  // An IME taking the Enter that confirms its text.
  await dispatch(new CompositionEvent("compositionstart", { bubbles: true }));
  await enter(true);
  await dispatch(new CompositionEvent("compositionend", { bubbles: true }));
  expect(request.mock.calls.some(([, method]) => method === "agent/send")).toBe(false);

  await enter(false);
  const send = request.mock.calls.find(([, method]) => method === "agent/send")!;
  expect(send[2]).toMatchObject({ runId, text: "Also mention the tests." });
  expect((send[2] as { turnId: string }).turnId).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7/);
  expect(box.textContent).toBe("");
  // Shown as pending until its turn starts.
  expect(transcriptText()).toContain("Also mention the tests.");
});

test("a dropped follow-up sent from here can be sent again, once", async () => {
  const { request, emit } = fakeBridge(4);
  await renderChat();
  const box = composer();
  type("Also mention the tests.");
  await act(async () => {
    box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });
  const sends = () => request.mock.calls.filter(([, method]) => method === "agent/send");
  const { turnId } = sends()[0]![2] as { turnId: string };
  emit({
    type: "event",
    event: {
      subscription: "s",
      seq: 50,
      time: "",
      event: { kind: "agent.output", runId, items: [{ kind: "followUpDropped", turnId }] },
    },
  });

  const sendAgain = () =>
    [...document.querySelectorAll("button")].find((b) => b.textContent === "Send again");
  await act(async () => sendAgain()!.click());
  expect(sends()).toHaveLength(2);
  expect(sends()[1]![2]).toMatchObject({ text: "Also mention the tests." });
  expect(sendAgain()).toBeUndefined();
});

test("Stop puts a first prompt's images back too, fetched from plxd by id", async () => {
  const { request, emit } = fakeBridge(2);
  await renderChat();
  // The first turn's turnStarted brings its images' ids, before the agent says anything.
  emit({
    type: "event",
    event: {
      subscription: "s",
      seq: 50,
      time: "",
      event: { kind: "agent.output", runId, items: [{ kind: "turnStarted", images: ["i-1"] }] },
    },
  });
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.click(),
  );
  await settle();
  expect(request).toHaveBeenCalledWith("local", "agent/image", { runId, imageId: "i-1" });
  expect(document.querySelector<HTMLImageElement>('img[alt="Image 1"]')!.src).toBe(
    "data:image/png;base64,AAAA",
  );
});

test("a follow-up Stop puts back in the box offers no Send again once plxd drops it", async () => {
  const { request, emit } = fakeBridge(4);
  await renderChat();
  type("Also mention the tests.");
  await act(async () => {
    composer().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });
  const send = request.mock.calls.find(([, method]) => method === "agent/send")!;
  const { turnId } = send[2] as { turnId: string };
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.click(),
  );
  expect(composer().textContent).toBe("Also mention the tests.");
  emit({
    type: "event",
    event: {
      subscription: "s",
      seq: 50,
      time: "",
      event: { kind: "agent.output", runId, items: [{ kind: "followUpDropped", turnId }] },
    },
  });
  expect([...document.querySelectorAll("button")].some((b) => b.textContent === "Send again")).toBe(
    false,
  );
});

test("a message a finished run couldn't take goes back in the box, with no loader", async () => {
  const send = async (text: string) => {
    type(text);
    await act(async () => {
      composer().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    await settle();
  };
  // plxd answers with the run its CLI failed to start again, and no turn follows.
  fakeBridge(8, { sent: { status: "failed", error: "the CLI didn't start" } });
  await renderChat();
  await send("Also mention the tests.");
  expect(composer().textContent).toBe("Also mention the tests.");
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("the CLI didn't start");
  expect(transcriptText()).not.toContain("Also mention the tests.");
  expect(document.querySelector(".loader")).toBeNull();
  act(() => unmount());

  fakeBridge(8, { sent: { status: "failed" } });
  await renderChat();
  await send("Also mention the tests.");
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("The agent couldn't start.");
});

test("a pasted image goes with agent/send beside the text, and shows while it's pending", async () => {
  vi.stubGlobal("createImageBitmap", async () => ({ width: 1, height: 1, close() {} }));
  const promptImages = { maxImages: 10, maxImageBytes: 5_242_880, maxTotalBytes: 6_291_456 };
  const { request } = fakeBridge(4, { capabilities: { promptImages } });
  await renderChat();
  const data = new DataTransfer();
  data.items.add(new File([Uint8Array.of(0x89, 0x50, 0x4e, 0x47)], "a.png", { type: "image/png" }));
  act(() => {
    composer().dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  });
  await act(() => new Promise((resolve) => setTimeout(resolve, 20)));
  await act(async () => {
    composer().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });

  const send = request.mock.calls.find(([, method]) => method === "agent/send")!;
  expect(send[2]).toMatchObject({
    runId,
    text: "",
    images: [{ mediaType: "image/png", data: "iVBORw==" }],
  });
  const img = document.querySelector('[role="log"] img');
  expect(img?.getAttribute("src")).toBe("data:image/png;base64,iVBORw==");
  vi.unstubAllGlobals();
});

test("Stop cancels, and a failed cancel says why and allows another try", async () => {
  const { request } = fakeBridge(4, { cancelError: "plxd is gone" });
  await renderChat();
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.click(),
  );
  expect(request).toHaveBeenCalledWith("local", "agent/cancel", { runId });
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("plxd is gone");
  expect(document.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.disabled).toBe(
    false,
  );
});

test("Stop or Esc before the agent answers puts the prompt back in the box, but not after", async () => {
  const prompt = "Add a README that explains how to build the app.";
  const stopButton = () => document.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!;
  fakeBridge(2); // started, with nothing from the agent yet
  await renderChat();
  await act(async () => stopButton().click());
  expect(composer().textContent).toBe(prompt);
  act(() => unmount());

  fakeBridge(2);
  await renderChat();
  await act(
    async () =>
      void composer().dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
  );
  expect(composer().textContent).toBe(prompt);
  act(() => unmount());

  const { request } = fakeBridge(4); // the agent has replied
  await renderChat();
  await act(async () => stopButton().click());
  expect(request).toHaveBeenCalledWith("local", "agent/cancel", { runId });
  expect(composer().textContent).toBe("");
});

test("a finished run opens a pull request titled like its thread, then links to it", async () => {
  const openPr = () =>
    [...document.querySelectorAll("button")].find((b) => b.textContent === "Open PR");
  // Not while the run goes, nor in a thread with no repo, nor from a plxd that can't.
  fakeBridge(4, { capabilities: { openPr: {} } });
  await renderChat();
  expect(openPr()).toBeUndefined();
  act(() => unmount());
  fakeBridge(8, { capabilities: { openPr: {} } });
  await renderChat(true);
  expect(openPr()).toBeUndefined();
  act(() => unmount());
  fakeBridge(8);
  await renderChat();
  expect(openPr()).toBeUndefined();
  act(() => unmount());

  const url = "https://github.com/me/app/pull/42";
  const { request } = fakeBridge(8, { capabilities: { openPr: {} }, prUrl: url });
  await renderChat();
  await act(async () => openPr()!.click());
  expect(request).toHaveBeenCalledWith("local", "agent/openPr", {
    runId,
    title: "Add a README that explains how to build the app.",
  });
  // An https link in a new window, which main opens in the browser.
  const link = document.querySelector<HTMLAnchorElement>(`a[href="${url}"]`)!;
  expect(link.textContent).toBe("PR #42");
  expect(link.target).toBe("_blank");
  expect(openPr()).toBeUndefined();
});

test("an Open PR that fails for gh says so in one line by Set up GitHub", async () => {
  fakeBridge(8, {
    capabilities: { openPr: {} },
    prError: {
      code: -32000,
      message: "GitHub CLI isn't installed on the host: gh was not found; looked in /usr/bin",
      data: { kind: "ghUnavailable" },
    },
  });
  const onSetUpGithub = vi.fn();
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
    unmount = () => {};
  };
  act(() => root.render(<AgentChat hostId="local" runId={runId} onSetUpGithub={onSetUpGithub} />));
  await settle();
  const button = (name: string) =>
    [...document.querySelectorAll("button")].find((b) => b.textContent === name);
  await act(async () => button("Open PR")!.click());
  expect(document.querySelector('[role="alert"]')!.textContent).toBe(
    "GitHub isn't installed on this host.Set up GitHub",
  );
  await act(async () => button("Set up GitHub")!.click());
  expect(onSetUpGithub).toHaveBeenCalledOnce();
});

test("with the PR view, Open PR opens it, a linked one's chip takes its place, and the view's messages reach the chat", async () => {
  const url = "https://github.com/me/app/pull/42";
  const { request } = fakeBridge(8, { capabilities: { openPr: {} }, prUrl: url });
  const onPrOpened = vi.fn();
  const onComposed = vi.fn();
  // One chat, drawn again with new props.
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
    unmount = () => {};
  };
  const chat = (props: Partial<ComponentProps<typeof AgentChat>>) =>
    act(() =>
      root.render(
        <AgentChat
          hostId="local"
          runId={runId}
          onPrOpened={onPrOpened}
          onComposed={onComposed}
          {...props}
        />,
      ),
    );
  chat({});
  await settle();
  await act(async () =>
    [...document.querySelectorAll("button")].find((b) => b.textContent === "Open PR")!.click(),
  );
  expect(onPrOpened).toHaveBeenCalledWith(url);
  expect(document.querySelector(`a[href="${url}"]`)).toBeNull();

  chat({ pullRequests: <button type="button">#42</button> });
  expect(document.body.textContent).toContain("#42");
  expect(document.body.textContent).not.toContain("Open PR");

  chat({ compose: { text: "Explain this pull request", send: true } });
  await settle();
  expect(request).toHaveBeenCalledWith(
    "local",
    "agent/send",
    expect.objectContaining({ runId, text: "Explain this pull request" }),
  );
  expect(onComposed).toHaveBeenCalledTimes(1);
  chat({ compose: { text: `${url} `, send: false } });
  await settle();
  expect(document.querySelector('[role="textbox"]')!.textContent).toBe(`${url} `);
  expect(onComposed).toHaveBeenCalledTimes(2);
});

test("while disconnected, nothing loads and the composer says why", async () => {
  const { request } = fakeBridge(8);
  window.parallax.connectionState = async () => ({
    status: "failed",
    retrying: true,
    error: { reason: "exited", message: "plxd exited" },
  });
  await renderChat();
  expect(request).not.toHaveBeenCalled();
  expect(composer().getAttribute("aria-placeholder")).toBe("Disconnected from plxd");
  const send = document.querySelector<HTMLButtonElement>('button[aria-label="Send"]')!;
  expect(send.disabled).toBe(true);
});

test("an open thread's composer picks within its provider and sends only what changed", async () => {
  const onSend = vi.fn(async () => undefined);
  const open = (optionsDisabled?: string) =>
    render(
      <Composer
        backend="claude"
        started={{ model: "claude-opus-5-5", effort: "xhigh", permission: "plan" }}
        unavailable={{
          Codex: "Codex is unavailable in this thread. Start a new thread to switch providers.",
        }}
        onSend={onSend}
        optionsDisabled={optionsDisabled}
      />,
    );
  const control = (label: string) => document.querySelector(`[aria-label="${label}"]`)!;
  // happy-dom doesn't disable a disabled fieldset's controls, which browsers do.
  const off = (label: string) => control(label).closest("fieldset")!.disabled;

  open("The model, effort, and access can change once it finishes");
  expect(off("Model: Claude Opus 5.5")).toBe(true);
  expect(off("Reasoning effort: Extra high")).toBe(true);
  expect(off("Access: Plan")).toBe(true);
  act(() => unmount());

  open();
  expect(off("Model: Claude Opus 5.5")).toBe(false);
  expect(off("Reasoning effort: Extra high")).toBe(false);
  expect(off("Access: Plan")).toBe(false);
  // The model menu lists only Claude's models; Codex is in the rail, disabled, saying why.
  const menu = document.getElementById(
    control("Model: Claude Opus 5.5").getAttribute("popovertarget")!,
  )!;
  const listed = [...menu.querySelectorAll<HTMLElement>('[role="menuitemradio"]')];
  expect(listed.every((m) => m.textContent?.includes("Claude"))).toBe(true);
  expect(menu.querySelector<HTMLButtonElement>('button[aria-label="Claude"]')!.disabled).toBe(
    false,
  );
  const codex = menu.querySelector<HTMLButtonElement>('button[aria-label="Codex"]')!;
  expect(codex.disabled).toBe(true);
  expect(codex.parentElement!.querySelector('[role="tooltip"]')!.textContent).toBe(
    "Codex is unavailable in this thread. Start a new thread to switch providers.",
  );

  await act(async () => listed.find((m) => m.textContent?.startsWith("Claude Sonnet 5"))!.click());
  const set = (el: HTMLInputElement, value: string) =>
    act(() => {
      Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!.set!.call(el, value);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
  set(document.querySelector<HTMLInputElement>('input[type="range"]')!, "0");
  type("Just the summary");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Send"]')!.click(),
  );
  // Access is still the run's, so it isn't sent.
  expect(onSend).toHaveBeenCalledWith(
    "Just the summary",
    { model: "claude-sonnet-5", effort: "low" },
    [],
  );
});

test("on a plxd that takes them, an open thread's context window and fast mode send only what changed", async () => {
  const onSend = vi.fn(async () => undefined);
  render(
    <Composer
      backend="claude"
      started={{ model: "claude-opus-5-5", effort: "high", contextWindow: 1_000_000 }}
      contextAndFast
      onSend={onSend}
    />,
  );
  const effort = () => document.querySelector('[aria-label^="Reasoning effort: "]')!;
  expect(effort().getAttribute("aria-label")).toBe("Reasoning effort: High · 1M");
  const choose = (menu: string, option: string) =>
    act(() =>
      [
        ...document.querySelectorAll<HTMLElement>(
          `[role="menu"][aria-label="${menu}"] [role="menuitemradio"]`,
        ),
      ]
        .find((o) => o.textContent === option)!
        .click(),
    );
  choose("Context window", "200K");
  choose("Fast mode", "On");
  expect(effort().textContent).toBe("High · 200K");
  expect(effort().getAttribute("aria-label")).toBe("Reasoning effort: High · 200K, fast");
  type("Quicker");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Send"]')!.click(),
  );
  expect(onSend).toHaveBeenCalledWith("Quicker", { contextWindow: 200_000, fast: true }, []);
});

test("another provider's model moves an open thread there, with every option and the account", async () => {
  const onSend = vi.fn(async () => undefined);
  render(
    <Composer
      backend="claude"
      started={{ model: "claude-opus-5-5", effort: "xhigh", permission: "plan" }}
      onSend={onSend}
    />,
  );
  const control = (label: string) => document.querySelector(`[aria-label="${label}"]`);
  const menu = document.getElementById(
    control("Model: Claude Opus 5.5")!.getAttribute("popovertarget")!,
  )!;
  expect(menu.querySelector<HTMLButtonElement>('button[aria-label="Codex"]')!.disabled).toBe(false);
  act(() => menu.querySelector<HTMLButtonElement>('button[aria-label="Codex"]')!.click());
  const astra = [...menu.querySelectorAll<HTMLElement>('[role="menuitemradio"]')].find((m) =>
    m.textContent?.startsWith("GPT-6 Astra"),
  )!;
  await act(async () => astra.click());
  // Codex has no Plan, so Plan becomes Accept Edits.
  expect(control("Access: Plan")).toBeNull();
  type("Carry on");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('button[aria-label="Send"]')!.click(),
  );
  expect(onSend).toHaveBeenCalledWith(
    "Carry on",
    {
      model: "gpt-6-astra",
      effort: "xhigh",
      permission: "edit",
      account: { kind: "subscription", backend: "codex" },
    },
    [],
  );
});

test("a thread's options change while it runs, and other providers are offered, on a plxd that moves runs", async () => {
  const tooltip = (provider: string) => {
    const model = document.querySelector('[aria-label^="Model: "]')!;
    const menu = document.getElementById(model.getAttribute("popovertarget")!)!;
    const rail = menu.querySelector<HTMLButtonElement>(`button[aria-label="${provider}"]`)!;
    return rail.disabled ? rail.parentElement!.querySelector('[role="tooltip"]')!.textContent : "";
  };
  const fieldset = () => document.querySelector('[aria-label^="Model: "]')!.closest("fieldset")!;

  // The sample's run is still going at seq 4.
  fakeBridge(4, { capabilities: { sendModel: {} } });
  await renderChat();
  expect(fieldset().disabled).toBe(true);
  expect(tooltip("Codex")).toBe(
    "Codex is unavailable in this thread. Start a new thread to switch providers.",
  );
  act(() => unmount());

  fakeBridge(4, { capabilities: { sendModel: {}, sendAccount: {} } });
  await renderChat();
  expect(fieldset().disabled).toBe(false);
  expect(tooltip("Codex")).toBe("");
  act(() => unmount());

  // A coordinator can't move to a backend that can't run one.
  fakeBridge(4, { capabilities: { sendModel: {}, sendAccount: {} } });
  const sample = [...logged];
  logged.splice(
    0,
    logged.length,
    ...sample.map((e) =>
      e.event.kind === "agent.started" && e.event.run
        ? { ...e, event: { ...e.event, run: { ...e.event.run, policy: "noWrite" as const } } }
        : e,
    ),
  );
  try {
    await renderChat();
    expect(tooltip("Codex")).toBe("Codex can't run a Project's coordinator yet.");
  } finally {
    logged.splice(0, logged.length, ...sample);
  }
});

test("while a run goes, only the last work row shows what the agent is doing", () => {
  const transcript = (rows: Item[]) => render(<TranscriptView rows={rows} sent={new Map()} live />);
  const user: Item = { kind: "user", key: "u", text: "go" };
  const tool: Item = {
    kind: "tool",
    key: "t",
    callId: "1",
    name: "Bash",
    input: { command: "ls" },
  };
  const reply: Item = { kind: "assistant", key: "a", text: "Hi", partial: true };
  const header = () => document.querySelector("button[aria-expanded]");

  // A reply with no tools needs no placeholder above it.
  transcript([user, reply]);
  expect(header()).toBeNull();
  act(() => unmount());

  // The loader for what it's doing, its label, and what it's doing it to.
  transcript([user, tool]);
  expect(header()!.textContent).toBe("Runningls");
  expect(header()!.querySelector('[data-loader="register"][data-variant="shift"]')).not.toBeNull();
  act(() => unmount());

  // Once its text streams, the work before it is done.
  transcript([user, { ...tool, at: "2026-01-01T00:00:00Z" }, reply]);
  expect(header()!.textContent).toBe("Worked briefly");
  expect(document.querySelector(".loader")).toBeNull();
});

test("until the agent does anything, a loader muses under the message on its way to it", () => {
  const user: Item = { kind: "user", key: "u", text: "go" };
  const reply: Item = { kind: "assistant", key: "a", text: "Done." };
  const end: Item = { kind: "end", key: "e", outcome: { status: "completed" } };
  const pending = { kind: "pending" as const, key: "pending:t", text: "And the tests?" };
  const fallback: Item = { kind: "notice", key: "n", tone: "info", text: "Switched accounts." };
  const transcript = (rows: Parameters<typeof TranscriptView>[0]["rows"], live: boolean) =>
    render(<TranscriptView rows={rows} sent={new Map()} live={live} />);
  // The empty work row's header: the loader and a word, which screen readers hear as Working.
  const musing = () => {
    const header = document.querySelector<HTMLButtonElement>("button[aria-expanded]");
    if (!header) return undefined;
    expect(header.disabled).toBe(true);
    expect(header.querySelector('[data-loader="orbit"][data-variant="chase"]')).not.toBeNull();
    expect(header.querySelector('[aria-hidden="true"]:not(.loader)')!.textContent).toMatch(
      /^[A-Z][a-z]+$/,
    );
    return header.querySelector(".sr-only")?.textContent;
  };
  // What follows the message, by row: a musing is a work row with nothing in it.
  const after = (text: string) => {
    const rows = [...document.querySelectorAll("[data-index]")];
    const i = rows.findIndex((r) => r.textContent === text);
    return rows.slice(i + 1).map((r) => r.textContent);
  };

  // A message the run has, but hasn't answered yet.
  transcript([user], true);
  expect(musing()).toBe("Working");
  act(() => unmount());

  // A message on its way to the agent, even before the run goes again, as a finished one resumes.
  transcript([user, reply, end, pending], false);
  expect(musing()).toBe("Working");
  expect(after("And the tests?")).toHaveLength(1);
  act(() => unmount());

  // The musing goes where the work will be, before a notice after the message.
  transcript([user, fallback], true);
  expect(musing()).toBe("Working");
  expect(after("go").at(-1)).toBe("Switched accounts.");
  act(() => unmount());

  // Not once the agent answered, nor for a message of a run that has stopped.
  transcript([user, reply], true);
  expect(musing()).toBeUndefined();
  act(() => unmount());
  transcript([user], false);
  expect(musing()).toBeUndefined();
  act(() => unmount());

  // Nor in a transcript that's out of date: no loader at all.
  render(<TranscriptView rows={[user, pending]} sent={new Map()} live stalled />);
  expect(musing()).toBeUndefined();
  expect(document.querySelector(".loader")).toBeNull();
});

test("the musing changes its word on the wall clock, while screen readers keep hearing Working", () => {
  // On a word's boundary: the first word is Picturing.
  vi.useFakeTimers({ now: 2400 * 8000 });
  render(<TranscriptView rows={[{ kind: "user", key: "u", text: "go" }]} sent={new Map()} live />);
  const header = () => document.querySelector("button[aria-expanded]")!;
  // Each word shown, and whether it fades in or out.
  const words = () =>
    [...header().querySelectorAll('[aria-hidden="true"]:not(.loader) > span')].map((w) => [
      w.textContent,
      w.className.match(/working-(in|out)/)?.[0],
    ]);
  // The first word doesn't fade in, so a remount doesn't blink.
  expect(words()).toEqual([["Picturing", undefined]]);
  act(() => void vi.advanceTimersByTime(2400));
  expect(words()).toEqual([
    ["Picturing", "working-out"],
    ["Pondering", "working-in"],
  ]);
  act(() => void vi.advanceTimersByTime(2400));
  expect(words()).toEqual([
    ["Pondering", "working-out"],
    ["Sketching", "working-in"],
  ]);
  expect(header().querySelector(".sr-only")!.textContent).toBe("Working");
  act(() => unmount());
  expect(vi.getTimerCount()).toBe(0);
});

test("under reduced motion, the musing keeps its word", () => {
  vi.useFakeTimers({ now: 2400 * 8000 });
  // appearance.ts sets it under the OS's Reduce motion or the app's own.
  document.documentElement.classList.add("reduce-motion");
  render(<TranscriptView rows={[{ kind: "user", key: "u", text: "go" }]} sent={new Map()} live />);
  expect(vi.getTimerCount()).toBe(0);
  act(() => void vi.advanceTimersByTime(4800));
  expect(document.querySelector("button[aria-expanded]")!.textContent).toBe("WorkingPicturing");
  document.documentElement.classList.remove("reduce-motion");
});

test("a running chat that loses plxd shows no loader", async () => {
  // The run is going, and the agent hasn't done anything yet.
  const { connect } = fakeBridge(2);
  await renderChat();
  expect(document.querySelector('[role="log"] .loader')).not.toBeNull();
  connect({ status: "connecting" });
  expect(document.querySelector(".loader")).toBeNull();
  connect({
    status: "failed",
    retrying: true,
    error: { reason: "exited", message: "plxd exited" },
  });
  expect(document.querySelector(".loader")).toBeNull();
  expect(transcriptText()).toContain("Add a README");
});

test("until its transcript loads, a chat shows its first message, with the loader only if it goes", async () => {
  fakeBridge(8);
  // The transcript is still on its way.
  window.parallax.request = vi.fn(() => new Promise<never>(() => {}));
  render(<AgentChat hostId="local" runId={runId} prompt="Add a README" />);
  await settle();
  expect(transcriptText()).toBe("Add a README");
  expect(document.querySelector(".loader")).toBeNull();
  act(() => unmount());

  render(<AgentChat hostId="local" runId={runId} prompt="Add a README" going />);
  await settle();
  expect(document.querySelector('[role="log"] .bg-selected')!.textContent).toBe("Add a README");
  expect(document.querySelector('[role="log"] button[aria-expanded] .sr-only')!.textContent).toBe(
    "Working",
  );
});

test("a chat that couldn't load shows its first message, with no loader", async () => {
  fakeBridge(8);
  window.parallax.request = vi.fn(async () => ({
    error: { code: -32000, message: "plxd is gone" },
  }));
  render(<AgentChat hostId="local" runId={runId} prompt="Add a README" going />);
  await settle();
  expect(transcriptText()).toBe("Add a README");
  expect(document.querySelector(".loader")).toBeNull();
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("plxd is gone");
});

// A two-step checklist with `done` steps finished and the next one under way.
const checklist = (done: number): AgentTodoItem[] =>
  ["Read", "Build"].map((text, i) => ({
    text,
    status: i < done ? "completed" : i === done ? "inProgress" : "pending",
  }));
// TodoWrite as plxd sends it: the call, then the checklist it stands for.
const todoWrite = (n: number, done: number): Item[] => [
  { kind: "tool", key: `w${n}`, callId: `w${n}`, name: "TodoWrite", input: {}, status: "ok" },
  { kind: "todo", key: `t${n}`, items: checklist(done) },
];

test("a turn's plan is one line where it began, its updates lines in the work, its proposal a card", () => {
  const rows: Item[] = [
    { kind: "user", key: "u", text: "go" },
    { kind: "reasoning", key: "r", text: "A plan first." },
    {
      kind: "tool",
      key: "x",
      callId: "x",
      name: "ExitPlanMode",
      input: { plan: "## Ship it\n\n- Read\n- Build" },
      status: "ok",
    },
    ...todoWrite(1, 0),
    { kind: "tool", key: "b", callId: "b", name: "Bash", input: { command: "ls" }, status: "ok" },
    ...todoWrite(2, 1),
    ...todoWrite(3, 2),
    { kind: "assistant", key: "a", text: "Done." },
  ];
  render(<TranscriptView rows={rows} sent={new Map()} live={false} />);
  const rowAt = (i: number) => document.querySelectorAll("[data-index]")[i]!;
  const text = (r: Element) => r.textContent!.replace(/\s+/g, "");
  expect([...document.querySelectorAll("[data-index]")].map(text)).toEqual([
    "go",
    "Workedbriefly",
    "ProposedplanShipitReadBuild",
    "Madeaplan2of2done",
    "Workedbriefly",
    "Done.",
  ]);
  // The work after the plan holds its updates, one line each, and no TodoWrite calls.
  act(() => rowAt(4).querySelector("button")!.click());
  const lines = [...rowAt(4).querySelectorAll("p")].map((p) => p.textContent);
  expect(lines.filter((t) => t?.includes("the plan"))).toEqual([
    "Updated the planFinished: Read · Started: Build",
    "Updated the planFinished: Build",
  ]);
  expect(rowAt(4).querySelectorAll("summary")).toHaveLength(1); // Bash's
  expect(transcriptText()).not.toContain("TodoWrite");
});

test("Claude Code's task tools make the same lines as TodoWrite, and their rows go", () => {
  // As plxd logs them from Claude Code 2.1.283: each call, with its result's text.
  const task = (
    key: string,
    name: string,
    input: Record<string, string>,
    output: string,
  ): Item => ({
    kind: "tool",
    key,
    callId: key,
    name,
    input,
    status: "ok",
    output,
  });
  const create = (key: string, id: string, subject: string) =>
    task(
      key,
      "TaskCreate",
      { subject, description: subject },
      `Task #${id} created successfully: ${subject}`,
    );
  const update = (key: string, taskId: string, status: string) =>
    task(key, "TaskUpdate", { taskId, status }, `Updated task #${taskId} status`);
  const rows: Item[] = [
    { kind: "user", key: "u", text: "go" },
    { kind: "reasoning", key: "r", text: "A plan first." },
    create("c1", "1", "Read"),
    create("c2", "2", "Build"),
    update("p1", "1", "in_progress"),
    { kind: "tool", key: "b", callId: "b", name: "Bash", input: { command: "ls" }, status: "ok" },
    update("p2", "1", "completed"),
    update("p3", "2", "in_progress"),
    task("l", "TaskList", {}, "#1 [completed] Read\n#2 [in_progress] Build"),
    update("p4", "2", "completed"),
    { kind: "assistant", key: "a", text: "Done." },
  ];
  render(<TranscriptView rows={rows} sent={new Map()} live={false} />);
  const rowAt = (i: number) => document.querySelectorAll("[data-index]")[i]!;
  const text = (r: Element) => r.textContent!.replace(/\s+/g, "");
  expect([...document.querySelectorAll("[data-index]")].map(text)).toEqual([
    "go",
    "Workedbriefly",
    "Madeaplan2of2done",
    "Workedbriefly",
    "Done.",
  ]);
  act(() => rowAt(3).querySelector("button")!.click());
  const lines = [...rowAt(3).querySelectorAll("p")].map((p) => p.textContent);
  expect(lines.filter((t) => t?.includes("the plan"))).toEqual([
    "Updated the planAdded: Build",
    "Updated the planStarted: Read",
    "Updated the planFinished: Read",
    "Updated the planStarted: Build",
    "Updated the planFinished: Build",
  ]);
  expect(rowAt(3).querySelectorAll("summary")).toHaveLength(1); // Bash's
  expect(transcriptText()).not.toMatch(/Task(Create|Update|List)/);
});

test("while a run goes, the work after a plan muses until it starts", () => {
  const rows: Item[] = [
    { kind: "user", key: "u1", text: "go" },
    { kind: "todo", key: "t1", items: checklist(0) },
    { kind: "assistant", key: "a", text: "Stopped there." },
    { kind: "user", key: "u2", text: "Go on", turnId: "t" },
    ...todoWrite(2, 1),
  ];
  render(<TranscriptView rows={rows} sent={new Map()} live />);
  // After the plan, a work row for what comes next.
  const last = [...document.querySelectorAll("[data-index]")].at(-1)!;
  expect(last.querySelector("button[aria-expanded] .sr-only")!.textContent).toBe("Working");
});

test("while the run works on a plan, a strip over the composer shows it, until a turn without one or the end", async () => {
  const strip = () => document.querySelector('section[aria-label="Plan"]');
  // The sample's first turn: running, with a checklist one of three done.
  const first = fakeBridge(4);
  await renderChat();
  expect(strip()!.querySelector("button")!.textContent).toBe(
    "In progress: Write README.md1 of 3 done",
  );
  // A follow-up starts a turn with no plan of its own (seq 5). Focus in the strip goes to the box.
  act(() => strip()!.querySelector("button")!.focus());
  first.emit({ type: "event", event: { subscription: "s", ...logged[4]! } });
  expect(strip()).toBeNull();
  expect(document.activeElement).toBe(composer());
  act(() => unmount());

  const second = fakeBridge(4);
  await renderChat();
  expect(strip()).not.toBeNull();
  // The run finishes (seq 8).
  second.emit({ type: "event", event: { subscription: "s", ...logged[7]! } });
  expect(strip()).toBeNull();
  act(() => unmount());

  // Losing plxd stalls it: no strip.
  const third = fakeBridge(4);
  await renderChat();
  expect(strip()).not.toBeNull();
  third.connect({ status: "connecting" });
  expect(strip()).toBeNull();
});

test("a bar beside the transcript for each prompt shows it and its reply, and scrolls back to it", () => {
  const scrollTo = vi.spyOn(HTMLElement.prototype, "scrollTo").mockImplementation(() => {});
  const rows: Item[] = [
    { kind: "user", key: "u1", text: "Add a **README**" },
    { kind: "assistant", key: "a1", text: "Started.\n\nStill going." },
    { kind: "assistant", key: "a2", text: "Wrote `README.md`.\n\nIt lists the commands." },
    // Parallax's wake-up isn't the user's, and the reply to it isn't the README's.
    { kind: "user", key: "w", text: "Subagents finished", wake: true },
    { kind: "assistant", key: "a3", text: "Merged the PR." },
    { kind: "user", key: "u2", text: null, turnId: "t2" },
  ];
  const sent = new Map([["t2", { text: "Now the tests", images: [] }]]);
  render(<TranscriptView rows={rows} sent={sent} live={false} />);
  const bars = [...document.querySelectorAll<HTMLElement>('nav[aria-label="Prompts"] button')];
  expect(bars.map((b) => b.getAttribute("aria-label"))).toEqual([
    "Go to prompt 1: Add a README",
    "Go to prompt 2: Now the tests",
  ]);
  // Scrolled to the end, the latest prompt is the one being read.
  expect(bars.map((b) => b.getAttribute("aria-current"))).toEqual([null, "true"]);
  // Only a hovered or focused bar stands out, not the one being read.
  expect(bars[1]!.firstElementChild!.className).toBe(bars[0]!.firstElementChild!.className);

  act(() => bars[0]!.focus());
  const card = document.querySelector('nav[aria-label="Prompts"] + [aria-hidden]')!;
  expect([...card.querySelectorAll("p")].map((p) => p.textContent)).toEqual([
    "Add a README",
    "Wrote README.md.",
  ]);
  act(() => bars[0]!.click());
  expect(scrollTo).toHaveBeenCalled();
  scrollTo.mockRestore();
});

test("one prompt has no rail", () => {
  render(
    <TranscriptView
      rows={[{ kind: "user", key: "u", text: "go" }]}
      sent={new Map()}
      live={false}
    />,
  );
  expect(document.querySelector('nav[aria-label="Prompts"]')).toBeNull();
});

test("scrolled up from the end, Scroll to end shows over the transcript, and goes back down", () => {
  const scrollTo = vi.spyOn(HTMLElement.prototype, "scrollTo").mockImplementation(() => {});
  render(
    <TranscriptView
      rows={[{ kind: "user", key: "u", text: "go" }]}
      sent={new Map()}
      live={false}
    />,
  );
  const end = () =>
    [...document.querySelectorAll("button")].find((b) => b.textContent === "Scroll to end");
  expect(end()).toBeUndefined();

  const log = document.querySelector<HTMLElement>('[role="log"]')!;
  Object.defineProperty(log, "scrollHeight", { configurable: true, value: 5000 });
  Object.defineProperty(log, "clientHeight", { configurable: true, value: 800 });
  act(() => {
    log.scrollTop = 1000;
    log.dispatchEvent(new Event("scroll"));
  });
  act(() => end()!.click());
  expect(scrollTo).toHaveBeenLastCalledWith({ top: 5000, behavior: "smooth" });
  expect(end()).toBeUndefined();

  // Back at the end, it stays gone.
  act(() => {
    log.scrollTop = 4200;
    log.dispatchEvent(new Event("scroll"));
  });
  expect(end()).toBeUndefined();
  scrollTo.mockRestore();
});

test("a web link gets its site's logo, or a globe, and other links none (PLX-330)", () => {
  expect(linkIcon("https://github.com/ryan-stoffel/parallax/pull/470")).toBe(GitHubLogo);
  expect(linkIcon("https://gist.github.com/x")).toBe(GitHubLogo);
  expect(linkIcon("https://linear.app/ryanstoffel/issue/PLX-330")).toBe(LinearLogo);
  expect(linkIcon("https://notgithub.com/x")).toBe(Globe);
  expect(linkIcon("http://localhost:5173")).toBe(Globe);
  expect(linkIcon("mailto:a@b.c")).toBeUndefined();
  expect(linkIcon("archived.md")).toBeUndefined();
  expect(linkIcon(undefined)).toBeUndefined();
});
