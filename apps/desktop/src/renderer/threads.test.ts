import { expect, test } from "vite-plus/test";

import samples from "../../../../crates/parallax-protocol/samples/v1/threads.json";
import type {
  AgentRun,
  EventsEventParams,
  ThreadStartResult,
  ParallaxEvent,
} from "../protocol/generated/protocol";
import {
  asksOf,
  emptyThreads,
  groupThreads,
  noRepo,
  threadsReducer,
  type ThreadsState,
} from "./threads";

const messages = samples as { id?: number; method?: string; params?: unknown; result?: unknown }[];
const events = messages
  .filter((m) => m.method === "events/event")
  .map((m) => (m.params as EventsEventParams).event);
const [parallaxAdded, threadStarted, scratchAdded, threadArchived, threadDeleted] = events as [
  ParallaxEvent,
  ParallaxEvent,
  ParallaxEvent,
  ParallaxEvent,
  ParallaxEvent,
];
const started = messages.find((m) => m.id === 4 && m.result)!.result as ThreadStartResult;
const quickChat = messages.find((m) => m.id === 5 && m.result)!.result as ThreadStartResult;

const apply = (state: ThreadsState, ...list: ParallaxEvent[]) =>
  list.reduce((s, event) => threadsReducer(s, { type: "event", event }), state);

test("repo.added, thread.started, and project.created add entries, and repeating one changes nothing", () => {
  const project = {
    id: "p-1",
    name: "parallax",
    repoPath: "/src/parallax",
    createdAt: "2026-09-26T12:00:00Z",
    updatedAt: "2026-09-26T12:00:00Z",
  };
  const created: ParallaxEvent = { kind: "project.created", project };
  const state = apply(emptyThreads, parallaxAdded, threadStarted, created);
  expect(apply(state, parallaxAdded, threadStarted, created)).toEqual(state);
  expect(state.repos.map((r) => r.name)).toEqual(["parallax"]);
  expect(state.threads.map((t) => t.id)).toEqual([started.thread.id]);
  expect(state.projects).toEqual([project]);
});

test("project.deleted drops the project and its runs, and keeps the rest", () => {
  const project = (id: string) => ({
    id,
    name: id,
    repoPath: "/src/parallax",
    createdAt: "2026-09-26T12:00:00Z",
    updatedAt: "2026-09-26T12:00:00Z",
  });
  const run = (id: string, inProject: string) => ({ id, project: inProject }) as AgentRun;
  const state: ThreadsState = {
    ...emptyThreads,
    projects: [project("p-1"), project("p-2")],
    runs: { a: run("a", "p-1"), b: run("b", "p-1"), c: run("c", "p-2"), t: run("t", "r-1") },
  };
  const after = apply(state, { kind: "project.deleted", project: "p-1" });
  expect(after.projects.map((p) => p.id)).toEqual(["p-2"]);
  expect(Object.keys(after.runs)).toEqual(["c", "t"]);
});

test("project.updated replaces the project with its new name and icon, and repeating it changes nothing", () => {
  const project = {
    id: "p-1",
    name: "parallax",
    repoPath: "/src/parallax",
    createdAt: "2026-09-26T12:00:00Z",
    updatedAt: "2026-09-26T12:00:00Z",
  };
  const other = { ...project, id: "p-2", name: "ember" };
  const state = apply(
    emptyThreads,
    { kind: "project.created", project },
    { kind: "project.created", project: other },
  );
  const renamed = { ...project, name: "parallax app", icon: { name: "rocket", color: "green" } };
  const updated: ParallaxEvent = { kind: "project.updated", project: renamed };
  const next = apply(state, updated);
  expect(next.projects).toEqual([renamed, other]);
  expect(apply(next, updated)).toEqual(next);
});

test("thread.updated replaces the thread, and thread.deleted removes it", () => {
  const archived = apply(emptyThreads, parallaxAdded, threadStarted, threadArchived);
  expect(archived.threads).toEqual([{ ...started.thread, archived: true }]);
  expect(apply(archived, threadDeleted).threads).toEqual([]);
});

test("a thread's title is the first line of its run's prompt", () => {
  const run = { ...started.run, prompt: "  Fix the flaky attach test.\nIt fails on CI.  " };
  const state = threadsReducer(emptyThreads, {
    type: "snapshot",
    projects: [],
    repos: [],
    threads: [started.thread],
    runs: [run],
  });
  expect(state.titles).toEqual({ [run.id]: "Fix the flaky attach test." });
});

test("a thread's own title wins over its run's, a later run list keeps it, and clearing it falls back", () => {
  const titled = { ...started.thread, title: "Flaky attach test" };
  let state = threadsReducer(emptyThreads, {
    type: "snapshot",
    projects: [],
    repos: [],
    threads: [titled],
    runs: [started.run],
  });
  expect(state.titles[titled.id]).toBe("Flaky attach test");
  state = threadsReducer(state, { type: "runs", runs: [started.run] });
  expect(state.titles[titled.id]).toBe("Flaky attach test");
  state = apply(state, { kind: "thread.updated", thread: { ...titled, title: "Renamed" } });
  expect(state.titles[titled.id]).toBe("Renamed");
  state = apply(state, { kind: "thread.updated", thread: started.thread });
  expect(state.titles[titled.id]).toBe(started.run.prompt.trim().split("\n")[0]);
});

test("runs are kept by id: a later list replaces the runs it has, and keeps the rest", () => {
  const other = { ...started.run, id: "other" };
  let state = threadsReducer(emptyThreads, {
    type: "snapshot",
    projects: [],
    repos: [],
    threads: [started.thread],
    runs: [started.run, other],
  });
  state = threadsReducer(state, { type: "runs", runs: [{ ...started.run, status: "failed" }] });
  expect(state.runs[started.run.id]?.status).toBe("failed");
  expect(state.runs["other"]).toEqual(other);
});

test("groups: repositories, then No Repo, newest first, archived apart", () => {
  // The quick chat's result arrives before the scratch entry's repo.added (seq 50).
  let state = apply(emptyThreads, parallaxAdded, threadStarted);
  state = apply(state, { kind: "thread.started", thread: quickChat.thread });
  const newer = { ...started.thread, id: "newer", createdAt: "2026-09-26T13:00:00Z" };
  state = apply(state, { kind: "thread.started", thread: newer });

  const byGroup = () =>
    groupThreads(state).groups.map((g) => [g.id, g.name, g.threads.map((t) => t.id)]);
  const parallax = started.thread.repo;
  expect(byGroup()).toEqual([
    [parallax, "parallax", ["newer", started.thread.id]],
    [noRepo, "No Repo", [quickChat.thread.id]],
  ]);
  // Once the scratch entry is known, it is still the one No Repo group, not a repository.
  state = apply(state, scratchAdded, threadArchived);
  expect(byGroup()).toEqual([
    [parallax, "parallax", ["newer"]],
    [noRepo, "No Repo", [quickChat.thread.id]],
  ]);
  expect(groupThreads(state).archived.map((t) => t.id)).toEqual([started.thread.id]);
});

test("a scope's events keep a run current, stamp when it changed, and track what it asks", () => {
  const run = { id: "r-1", prompt: "Go", status: "running", updatedAt: "2026-10-01T10:00:00Z" };
  let state = threadsReducer(emptyThreads, {
    type: "snapshot",
    projects: [],
    repos: [],
    threads: [],
    runs: [run as never],
  });
  const asked = {
    seq: 5,
    time: "2026-10-01T10:01:00Z",
    event: {
      kind: "agent.output",
      runId: "r-1",
      items: [{ kind: "approvalRequested", requestId: "q-1", toolName: "Bash", input: {} }],
    },
  };
  const finished = {
    seq: 6,
    time: "2026-10-01T10:02:00Z",
    event: { kind: "agent.updated", runId: "r-1", state: { status: "completed", accountId: "a" } },
  };
  state = threadsReducer(state, { type: "scope", events: [asked as never] });
  expect(asksOf(state, "r-1")).toBe(1);
  state = threadsReducer(state, { type: "scope", events: [finished as never] });
  expect(state.runs["r-1"]).toMatchObject({
    status: "completed",
    updatedAt: "2026-10-01T10:02:00Z",
  });
});

test("an older log read for its requests never undoes a run's newer status", () => {
  const done = { id: "r-1", prompt: "Go", status: "completed", updatedAt: "2026-10-01T10:05:00Z" };
  let state = threadsReducer(emptyThreads, {
    type: "snapshot",
    projects: [],
    repos: [],
    threads: [],
    runs: [done as never],
  });
  const old = {
    seq: 2,
    time: "2026-10-01T10:00:00Z",
    event: { kind: "agent.updated", runId: "r-1", state: { status: "running", accountId: "a" } },
  };
  state = threadsReducer(state, { type: "approvals", events: [old as never] });
  expect(state.runs["r-1"]).toEqual(done);
});
