// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, test, vi } from "vite-plus/test";

import samples from "../../../../crates/parallax-protocol/samples/v1/agents.json";
import type { ParallaxBridge } from "../preload/bridge";
import type { AgentRun, GitStatus, LoggedEvent } from "../protocol/generated/protocol";
import { gitActions, GitMenu } from "./GitMenu";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// happy-dom has no popovers. The menu is in the DOM either way.
HTMLElement.prototype.hidePopover = () => {};
HTMLElement.prototype.showPopover = () => {};

const logged = (samples as { method?: string; params?: unknown }[])
  .filter((m) => m.method === "events/event")
  .map((m) => m.params as LoggedEvent);
// The sample's run, as it started (running, in a worktree, with no commit yet).
const started = (logged[0]!.event as { run: AgentRun }).run;
const finished: AgentRun = {
  ...started,
  status: "completed",
  diff: { commit: "abc", files: 1, insertions: 1, deletions: 0 },
};
const clean: GitStatus = {
  branch: "parallax/readme",
  changes: 0,
  upstream: "origin/parallax/readme",
  ahead: 0,
  origin: true,
};

test("the main action is the next useful one, and each other says why it can't run", () => {
  const wait = "Wait for the turn to end";
  expect(gitActions(started, clean, true)).toEqual({
    why: { commit: wait, push: wait, pr: wait },
    main: "commit",
  });
  expect(gitActions(finished, { ...clean, changes: 2 }, true)).toEqual({
    why: { push: "Nothing to push" },
    main: "commit",
    note: undefined,
  });
  expect(gitActions(finished, { ...clean, ahead: 1 }, true).main).toBe("push");
  expect(gitActions(finished, clean, true)).toEqual({
    why: { commit: "No changes", push: "Nothing to push" },
    main: "pr",
    note: undefined,
  });
  // A worktree run with no commit has no pull request to open, nor does a plxd without openPr.
  expect(gitActions({ ...finished, diff: undefined }, clean, true).why.pr).toBe("No commits yet");
  expect(gitActions(finished, clean, false).why.pr).toMatch(/can't open pull requests/);
  expect(gitActions(finished, { ...clean, origin: false }, true).why).toEqual({
    commit: "No changes",
    push: "No origin remote",
    pr: "No origin remote",
  });
  expect(gitActions(finished, { ...clean, branch: null, changes: 1 }, true)).toEqual({
    why: { push: "Detached HEAD", pr: "Detached HEAD" },
    main: "commit",
    note: "Detached HEAD: check out a branch to push or open a pull request.",
  });
});

/** A bridge whose plxd has `capabilities` and answers the Git methods with `answers`. */
function fakeBridge(
  capabilities: Record<string, object>,
  answers: Record<string, () => object> = {},
) {
  const request = vi.fn(async (_host: string, method: string) => {
    const answer = answers[method];
    return answer ? { ...answer(), logId: "log-1" } : { result: {}, logId: "log-1" };
  });
  window.parallax = {
    platform: "darwin",
    connectionState: async () => ({
      status: "connected",
      plxd: "0.1.0",
      protocol: 1,
      capabilities,
    }),
    onConnectionState: () => () => {},
    request,
  } as Partial<ParallaxBridge> as ParallaxBridge;
  return request;
}

let root: Root | undefined;
afterEach(() => {
  act(() => root?.unmount());
  root = undefined;
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

const settle = async () => {
  for (let i = 0; i < 10; i++) await act(async () => {});
};

async function render(run: AgentRun) {
  root ??= createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root!.render(<GitMenu hostId="local" run={run} />));
  await settle();
}

const button = (name: string) =>
  [...document.querySelectorAll("button")].find((b) => b.textContent === name);
const item = (name: string) =>
  [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find((b) =>
    b.textContent?.startsWith(name),
  )!;

test("nothing shows from a plxd without the git capability", async () => {
  fakeBridge({ openPr: {} });
  await render(finished);
  expect(document.body.textContent).toBe("");
});

test("Commit asks for a message prefilled from the title, then the menu shows the new status", async () => {
  let status = { ...clean, changes: 2 };
  const request = fakeBridge(
    { git: {}, openPr: {} },
    {
      "agent/gitStatus": () => ({ result: status }),
      "agent/commit": () => ({ result: (status = { ...clean, ahead: 1 }) }),
    },
  );
  await render(finished);
  expect(request).toHaveBeenCalledWith("local", "agent/gitStatus", { runId: finished.id });
  expect(item("Push").disabled).toBe(true);
  expect(item("Push").textContent).toContain("Nothing to push");

  await act(async () => button("Commit")!.click());
  const message = document.querySelector<HTMLTextAreaElement>(
    'textarea[aria-label="Commit message"]',
  )!;
  expect(message.value).toBe("Add a README that explains how to build the app.");
  await act(async () => document.querySelector("form")!.requestSubmit());
  expect(request).toHaveBeenCalledWith("local", "agent/commit", {
    runId: finished.id,
    message: "Add a README that explains how to build the app.",
  });
  // Committed: Push is next.
  expect(button("Push")).toBeDefined();
  expect(item("Commit").textContent).toContain("No changes");
});

test("a failed push says why in the menu", async () => {
  fakeBridge(
    { git: {}, openPr: {} },
    {
      "agent/gitStatus": () => ({ result: { ...clean, ahead: 1 } }),
      "agent/push": () => ({
        error: { code: -32000, message: "could not push", data: { kind: "pushFailed" } },
      }),
    },
  );
  await render(finished);
  await act(async () => button("Push")!.click());
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("could not push");
});

test("Create PR opens the pull request in the browser and reads the status again", async () => {
  const url = "https://github.com/me/app/pull/42";
  const open = vi.fn();
  vi.stubGlobal("open", open);
  const request = fakeBridge(
    { git: {}, openPr: {} },
    {
      "agent/gitStatus": () => ({ result: { ...clean, upstream: null, ahead: 1 } }),
      "agent/openPr": () => ({ result: { url } }),
    },
  );
  await render(finished);
  await act(async () => item("Create PR").click());
  expect(request).toHaveBeenCalledWith("local", "agent/openPr", {
    runId: finished.id,
    title: "Add a README that explains how to build the app.",
  });
  expect(open).toHaveBeenCalledWith(url, "_blank");
  expect(request.mock.calls.filter(([, m]) => m === "agent/gitStatus")).toHaveLength(2);
});

test("with onPrOpened, Create PR opens the PR view in place of the browser", async () => {
  const url = "https://github.com/me/app/pull/42";
  const open = vi.fn();
  vi.stubGlobal("open", open);
  fakeBridge(
    { git: {}, openPr: {}, pullRequests: {} },
    {
      "agent/gitStatus": () => ({ result: { ...clean, upstream: null, ahead: 1 } }),
      "agent/openPr": () => ({ result: { url } }),
    },
  );
  const onPrOpened = vi.fn();
  root ??= createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root!.render(<GitMenu hostId="local" run={finished} onPrOpened={onPrOpened} />));
  await settle();
  await act(async () => item("Create PR").click());
  expect(onPrOpened).toHaveBeenCalledWith(url);
  expect(open).not.toHaveBeenCalled();
});

test("a Create PR that fails for gh offers Set up GitHub, and another failure doesn't", async () => {
  let kind = "ghUnavailable";
  fakeBridge(
    { git: {}, openPr: {} },
    {
      "agent/gitStatus": () => ({ result: { ...clean, upstream: null, ahead: 1 } }),
      "agent/openPr": () => ({
        error: { code: -32000, message: "GitHub CLI isn't signed in", data: { kind } },
      }),
    },
  );
  const onSetUpGithub = vi.fn();
  root ??= createRoot(document.body.appendChild(document.createElement("div")));
  act(() => root!.render(<GitMenu hostId="local" run={finished} onSetUpGithub={onSetUpGithub} />));
  await settle();
  await act(async () => item("Create PR").click());
  expect(document.querySelector('[role="alert"]')!.textContent).toBe("GitHub CLI isn't signed in");
  await act(async () => button("Set up GitHub")!.click());
  expect(onSetUpGithub).toHaveBeenCalledOnce();

  kind = "prFailed";
  await act(async () => item("Create PR").click());
  expect(button("Set up GitHub")).toBeUndefined();
});

test("the status is read again when a turn ends, and every action waits while one runs", async () => {
  const request = fakeBridge(
    { git: {}, openPr: {} },
    { "agent/gitStatus": () => ({ result: clean }) },
  );
  await render(started);
  for (const name of ["Commit", "Push", "Create PR"]) {
    expect(item(name).disabled).toBe(true);
    expect(item(name).textContent).toContain("Wait for the turn to end");
  }
  await render(finished);
  expect(request.mock.calls.filter(([, m]) => m === "agent/gitStatus")).toHaveLength(2);
  expect(button("Create PR")!.disabled).toBe(false);
});

test("a detached HEAD shows its note", async () => {
  fakeBridge(
    { git: {}, openPr: {} },
    { "agent/gitStatus": () => ({ result: { ...clean, branch: null, upstream: null } }) },
  );
  await render(finished);
  expect(document.body.textContent).toContain(
    "Detached HEAD: check out a branch to push or open a pull request.",
  );
  expect(item("Push").disabled).toBe(true);
  expect(item("Create PR").disabled).toBe(true);
});
