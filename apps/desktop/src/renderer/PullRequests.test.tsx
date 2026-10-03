// @vitest-environment happy-dom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";

import diffSamples from "../../../../crates/parallax-protocol/samples/v1/pr-diff.json";
import samples from "../../../../crates/parallax-protocol/samples/v1/pull-requests.json";
import type { ParallaxBridge } from "../preload/bridge";
import type { PrDiffResult, PullRequest } from "../protocol/generated/protocol";
import {
  parseDiff,
  PullRequestChip,
  PullRequestList,
  PullRequestView,
  usePullRequests,
} from "./PullRequests";
import { SidePanel } from "./SidePanel";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// happy-dom has no popovers. The menus are in the DOM either way.
HTMLElement.prototype.hidePopover = () => {};

// The sample's pull request: open, two checks with one pending, and one comment.
const sample = (samples as { id?: number; result?: PullRequest }[]).find(
  (m) => m.id === 2 && m.result,
)!.result!;
const url = (n: number) => `https://github.com/me/app/pull/${n}`;
const prOf = (n: number, more: Partial<PullRequest> = {}): PullRequest => ({
  ...sample,
  number: n,
  url: url(n),
  title: `Change ${n}`,
  ...more,
});
// The merged one with a commit and every kind of review, and its diff.
const merged = (diffSamples as { id?: number; result?: PullRequest }[]).find(
  (m) => m.id === 1 && m.result,
)!.result!;
const sampleDiff = (diffSamples as { id?: number; result?: PrDiffResult }[]).find(
  (m) => m.id === 2 && m.result,
)!.result!;
const now = Date.parse("2026-10-02T13:10:00Z");

let read: Record<string, PullRequest>;
let actions: Record<string, () => object>;
let diffs: Record<string, PrDiffResult>;
const request = vi.fn(async (_host: string, method: string, params: Record<string, unknown>) => {
  if (method === "pr/view") return { logId: "l", result: read[params["url"] as string] };
  if (method === "pr/act") return { logId: "l", ...actions[params["action"] as string]!() };
  if (method === "pr/diff") return { logId: "l", result: diffs[params["url"] as string] };
  return { logId: "l", result: {} };
});

beforeEach(() => {
  vi.useFakeTimers({ now, toFake: ["Date"] });
  request.mockClear();
  read = {};
  actions = {};
  diffs = {};
  window.parallax = { platform: "darwin", request } as Partial<ParallaxBridge> as ParallaxBridge;
});

let root: Root | undefined;
afterEach(() => {
  act(() => root?.unmount());
  root = undefined;
  document.body.innerHTML = "";
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const settle = async () => {
  for (let i = 0; i < 10; i++) await act(async () => {});
};
const click = async (element: Element | null | undefined) => {
  await act(async () => (element as HTMLElement).click());
  await settle();
};
const button = (name: string) =>
  [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) =>
    b.textContent?.startsWith(name),
  );
const item = (name: string) =>
  [...document.querySelectorAll<HTMLButtonElement>('[aria-label="Pull request"] button')].find(
    (b) => b.textContent?.startsWith(name),
  )!;

/** Reads `urls` for run-1 and renders what `view` makes of them. */
async function render(
  urls: string[],
  view: (prs: ReturnType<typeof usePullRequests>) => React.ReactNode,
) {
  function Harness() {
    return <>{view(usePullRequests("local", "run-1", urls, true))}</>;
  }
  root ??= createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root!.render(<Harness />));
  await settle();
}

test("the chip shows the latest linked pull request in its state's color, and +k for the rest", async () => {
  read = { [url(41)]: prOf(41), [url(42)]: prOf(42, { state: "merged" }) };
  const onOpen = vi.fn();
  await render([url(42)], (prs) => <PullRequestChip prs={prs} onOpen={onOpen} />);
  expect(request).toHaveBeenCalledWith("local", "pr/view", { runId: "run-1", url: url(42) });
  let chip = document.querySelector("button")!;
  expect(chip.textContent).toBe("#42");
  expect(chip.querySelector("span")!.className).toContain("text-project-violet");
  // One pull request opens its view.
  await click(chip);
  expect(onOpen).toHaveBeenLastCalledWith(url(42));

  // Several open the list; the latest leads, and a draft is gray.
  read[url(43)] = prOf(43, { draft: true });
  await render([url(41), url(42), url(43)], (prs) => <PullRequestChip prs={prs} onOpen={onOpen} />);
  chip = document.querySelector("button")!;
  expect(chip.textContent).toBe("#43+2");
  expect(chip.querySelector("span")!.className).toContain("text-faint-foreground");
  await click(chip);
  expect(onOpen).toHaveBeenLastCalledWith(undefined);
});

test("the list shows each pull request newest first, with the open and linked counts", async () => {
  read = {
    [url(40)]: prOf(40, { state: "closed" }),
    [url(41)]: prOf(41, { additions: 1234, deletions: 5, updatedAt: "2026-10-02T08:10:00Z" }),
  };
  const onOpen = vi.fn();
  await render([url(40), url(41)], (prs) => <PullRequestList prs={prs} onOpen={onOpen} />);
  const rows = [...document.querySelectorAll("li")].map((li) => li.textContent);
  expect(rows).toEqual([
    "#41Change 41+1,234 −5meme/appparallax/add-readme → main5h ago",
    "#40Change 40+12 −0meme/appparallax/add-readme → main1h ago",
  ]);
  expect(document.querySelector("p")!.textContent).toBe("1 open · 2 linked · synced just now");
  await click(document.querySelectorAll("li button")[1]);
  expect(onOpen).toHaveBeenCalledWith(url(40));
});

test("the view shows the header, the checks' state, reviewers, labels, description, and comments newest first", async () => {
  read = {
    [url(42)]: prOf(42, {
      comments: [
        { author: "first", body: "Older.", createdAt: "2026-10-02T12:00:00Z" },
        { author: "second", body: "Line\n".repeat(20), createdAt: "2026-10-02T13:00:00Z" },
      ],
    }),
  };
  await render([url(42)], (prs) => <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />);
  const header = document.querySelector("header")!.textContent;
  expect(header).toContain("me/app#42");
  expect(header).toContain("Merge");
  expect(header).toContain("Change 42");
  expect(header).toContain("me · updated 1h ago");
  expect(header).toContain("main ← parallax/add-readme");
  expect(header).toContain("1 file+12−0");
  expect(document.body.textContent).toContain("1 of 2 running");
  expect(document.querySelector("dl")!.textContent).toBe("Reviewersreviewer, docs-teamLabelsdocs");
  expect(document.body.textContent).toContain("Explains how to build the app.");

  // The checks' state opens every check.
  const checks = document.querySelector('[aria-label="Checks"]')!;
  expect(button("1 of 2 running")!.getAttribute("popovertarget")).toBe(checks.id);
  expect([...checks.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
    "buildsuccessDetails",
    "testpending",
  ]);

  const comments = [...document.querySelectorAll("section:last-of-type li")];
  expect(comments.map((c) => c.querySelector("p")!.textContent)).toEqual([
    "second 10m ago",
    "first 1h ago",
  ]);
  // The long one is folded until asked.
  expect(comments[0]!.querySelector(".max-h-48")).not.toBeNull();
  expect(comments[1]!.textContent).not.toContain("Show full comment");
  await click(button("Show full comment"));
  expect(comments[0]!.querySelector(".max-h-48")).toBeNull();
});

test("auto-merge on shows in the Merge button, which turns it off", async () => {
  read = { [url(42)]: prOf(42, { autoMerge: "merge" }) };
  actions = { disableAutoMerge: () => ({ result: prOf(42) }) };
  await render([url(42)], (prs) => <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />);
  await click(button("Auto-merge (merge)"));
  expect(request).toHaveBeenCalledWith("local", "pr/act", {
    runId: "run-1",
    url: url(42),
    action: "disableAutoMerge",
  });
  expect(button("Merge")).toBeDefined();
});

test("the menu acts on the pull request, shows what GitHub has after, and says why one failed", async () => {
  read = { [url(42)]: prOf(42) };
  actions = {
    draft: () => ({ result: prOf(42, { draft: true }) }),
    autoMerge: () => ({ result: prOf(42, { autoMerge: "merge" }) }),
    squash: () => ({
      error: { code: -32000, message: "not mergeable", data: { kind: "prFailed" } },
    }),
  };
  const onCompose = vi.fn();
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  const open = vi.fn();
  vi.stubGlobal("open", open);
  await render([url(42)], (prs) => (
    <PullRequestView url={url(42)} prs={prs} onCompose={onCompose} />
  ));

  await click(item("Convert to draft"));
  expect(item("Ready for review")).toBeDefined();
  await click(item("Enable auto-merge"));
  expect(button("Auto-merge (merge)")).toBeDefined();
  await click(item("Squash and merge"));
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("not mergeable");

  read[url(42)] = prOf(42, { title: "Renamed" });
  await click(item("Refresh"));
  expect(document.querySelector("h2")!.textContent).toBe("Renamed");
  expect(document.querySelector('[role="alert"]')).toBeNull();

  await click(item("Ask a question"));
  expect(onCompose).toHaveBeenLastCalledWith(`${url(42)} `, false);
  await click(item("Explain this PR"));
  expect(onCompose).toHaveBeenLastCalledWith(expect.stringContaining(url(42)), true);
  await click(item("Fix findings in this thread"));
  expect(onCompose).toHaveBeenLastCalledWith(expect.stringContaining(url(42)), true);

  await click(item("Copy link"));
  expect(writeText).toHaveBeenLastCalledWith(url(42));
  await click(item("Copy PR number"));
  expect(writeText).toHaveBeenLastCalledWith("42");
  await click(item("Open on GitHub"));
  expect(open).toHaveBeenCalledWith(url(42), "_blank");
});

test("Close asks first, then closes", async () => {
  read = { [url(42)]: prOf(42) };
  actions = { close: () => ({ result: prOf(42, { state: "closed" }) }) };
  const showModal = vi.fn();
  HTMLDialogElement.prototype.showModal = showModal;
  await render([url(42)], (prs) => <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />);
  await click(item("Close pull request"));
  expect(showModal).toHaveBeenCalled();
  expect(request.mock.calls.some(([, m]) => m === "pr/act")).toBe(false);
  const dialog = document.querySelector("dialog")!;
  await click(
    [...dialog.querySelectorAll("button")].find((b) => b.textContent === "Close pull request"),
  );
  expect(request).toHaveBeenCalledWith("local", "pr/act", {
    runId: "run-1",
    url: url(42),
    action: "close",
  });
  // Closed: no Merge, and its state in its place.
  expect(document.querySelector("header")!.textContent).toContain("Closed");
  expect(document.querySelector('[aria-label="Merge options"]')).toBeNull();
});

test("a pull request that can't be read for gh says so in one line by Set up GitHub", async () => {
  request.mockImplementationOnce(async () => ({
    logId: "l",
    error: {
      code: -32000,
      message: "GitHub CLI isn't signed in on the host; run `gh auth login` there: not logged in",
      data: { kind: "ghUnavailable" },
    },
  }));
  const onSetUpGithub = vi.fn();
  await render([url(42)], (prs) => (
    <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} onSetUpGithub={onSetUpGithub} />
  ));
  expect(document.querySelector('[role="alert"]')!.textContent).toBe(
    "GitHub isn't signed in on this host.",
  );
  await click(button("Set up GitHub"));
  expect(onSetUpGithub).toHaveBeenCalledOnce();
});

test("the side panel opens a pull request's tab, or the list, and hides tabs the thread doesn't link", async () => {
  root = createRoot(document.body.appendChild(document.createElement("div")));
  const panel = (urls: string[], pullRequest?: { url?: string }) =>
    act(() =>
      root!.render(
        <SidePanel
          open
          onClose={() => {}}
          expanded={false}
          onExpandedChange={() => {}}
          pullRequest={pullRequest}
          pullRequests={{
            urls,
            list: <p>the list</p>,
            view: (u) => <p>view of {u}</p>,
          }}
        />,
      ),
    );
  const tabs = () =>
    [...document.querySelectorAll('[aria-label="Open views"] button[id]')].map(
      (t) => `${t.textContent}${t.getAttribute("aria-current") === "true" ? "*" : ""}`,
    );
  const shown = () => document.querySelector("#side-panel > div:not(.titlebar):not([hidden])");

  panel([url(41), url(42)]);
  panel([url(41), url(42)], { url: url(42) });
  expect(tabs()).toEqual(["#42*"]);
  expect(shown()!.textContent).toBe(`view of ${url(42)}`);
  panel([url(41), url(42)], {});
  expect(tabs()).toEqual(["#42", "Pull requests*"]);
  expect(shown()!.textContent).toBe("the list");
  // Another thread: its own pull requests only.
  panel([url(7)], {});
  expect(tabs()).toEqual(["Pull requests*"]);
});

test("the timeline shows opening, commits, comments, reviews, and the merge, newest first or oldest", async () => {
  read = {
    [url(42)]: {
      ...merged,
      comments: [{ author: "reviewer", body: "Nice.", createdAt: "2026-10-02T12:14:00Z" }],
    },
  };
  await render([url(42)], (prs) => <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />);
  await click(document.querySelector('[role="tab"]:nth-child(2)'));
  const rows = () =>
    [...document.querySelectorAll('[aria-label="Timeline"] > li')].map(
      (li) => li.querySelector("p")!.textContent,
    );
  expect(rows()).toEqual([
    "reviewer merged parallax/add-readme into main",
    "bot had a review dismissed",
    "bot reviewed",
    "bot requested changes",
    "reviewer approved these changes",
    "reviewer commented",
    "me opened this pull request",
    "docs: add a README",
  ]);
  await click(button("Newest first"));
  expect(rows()[0]).toBe("docs: add a README");
});

test("the code tab reads the diff once and shows each file numbered, folding a viewed one", async () => {
  read = { [url(42)]: merged };
  diffs = { [url(42)]: sampleDiff };
  await render([url(42)], (prs) => <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />);
  expect(request.mock.calls.some(([, m]) => m === "pr/diff")).toBe(false);
  await click(document.querySelector('[role="tab"]:nth-child(3)'));
  expect(request).toHaveBeenCalledWith("local", "pr/diff", { runId: "run-1", url: url(42) });
  expect(document.body.textContent).toContain("1 file · 0 / 1 viewed");
  const file = document.querySelector('[aria-label="Changed files"] li')!;
  expect(file.textContent).toContain("README.md+2 −0");
  expect(file.textContent).toContain("1+Added: # App");
  await click(file.querySelector('input[type="checkbox"]'));
  expect(document.body.textContent).toContain("1 file · 1 / 1 viewed");
  expect(file.textContent).not.toContain("# App");

  // Back again: the same diff, not read again.
  await click(document.querySelector('[role="tab"]:nth-child(1)'));
  await click(document.querySelector('[role="tab"]:nth-child(3)'));
  expect(request.mock.calls.filter(([, m]) => m === "pr/diff")).toHaveLength(1);
});

test("a re-render while the diff is read doesn't read it again", async () => {
  read = { [url(42)]: merged };
  let answer!: (value: { logId: string; result: PrDiffResult }) => void;
  request.mockImplementation(async (_host, method, params) => {
    if (method === "pr/diff")
      return new Promise<{ logId: string; result: PrDiffResult }>((resolve) => (answer = resolve));
    return { logId: "l", result: read[params["url"] as string] };
  });
  let rerender!: () => void;
  function Harness() {
    const [, set] = useState(0);
    rerender = () => set((n) => n + 1);
    const prs = usePullRequests("local", "run-1", [url(42)], true);
    return <PullRequestView url={url(42)} prs={prs} onCompose={vi.fn()} />;
  }
  root = createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root!.render(<Harness />));
  await settle();
  await click(document.querySelector('[role="tab"]:nth-child(3)'));
  act(() => rerender());
  await settle();
  await act(async () => answer({ logId: "l", result: sampleDiff }));
  await settle();
  expect(request.mock.calls.filter(([, m]) => m === "pr/diff")).toHaveLength(1);
  expect(document.body.textContent).toContain("1 file · 0 / 1 viewed");
});

test("parseDiff numbers each side, and keeps renames and binary files", () => {
  const files = parseDiff(
    [
      "diff --git a/a.txt b/a.txt",
      "index 1..2 100644",
      "--- a/a.txt",
      "+++ b/a.txt",
      "@@ -10,3 +10,3 @@ fn main",
      " keep",
      "--- gone",
      "+++ here",
      "\\ No newline at end of file",
      "diff --git a/old.png b/new.png",
      "rename from old.png",
      "rename to new.png",
      "Binary files a/old.png and b/new.png differ",
      "",
    ].join("\n"),
  );
  expect(files).toEqual([
    {
      path: "a.txt",
      added: 1,
      removed: 1,
      binary: false,
      lines: [
        { op: "@", text: "@@ -10,3 +10,3 @@ fn main" },
        { op: " ", text: "keep", old: 10, new: 10 },
        { op: "-", text: "-- gone", old: 11 },
        { op: "+", text: "++ here", new: 11 },
      ],
    },
    { path: "new.png", oldPath: "old.png", added: 0, removed: 0, binary: true, lines: [] },
  ]);
  // A path with ` b/` in it, and one git quotes.
  expect(
    parseDiff(
      [
        "diff --git a/x b/y.bin b/x b/y.bin",
        "Binary files a/x b/y.bin and b/x b/y.bin differ",
        'diff --git "a/caf\\303\\251.md" "b/caf\\303\\251.md"',
        '--- "a/caf\\303\\251.md"',
        '+++ "b/caf\\303\\251.md"',
      ].join("\n"),
    ).map((f) => f.path),
  ).toEqual(["x b/y.bin", "caf\\303\\251.md"]);
});
