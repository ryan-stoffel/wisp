import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";

import type { RpcError, ThreadName } from "../preload/bridge";
import type {
  AgentRun,
  LoggedEvent,
  Project,
  ProjectIcon as ProjectIconValue,
  ProjectStartParams,
  ProjectUpdateParams,
  PromptImage,
  Repo,
  Thread,
  ParallaxEvent,
} from "../protocol/generated/protocol";
import { describeError } from "./errors";
import type { RunOptions } from "./models";
import { isRunning, trackApprovals, updateRun, type ApprovalsByRun } from "./transcript";
import { uuidv7 } from "./uuidv7";

/** A host's projects, repo entries, and normal threads (0017), and each thread's title and run. */
export interface ThreadsState {
  projects: Project[];
  repos: Repo[];
  threads: Thread[];
  /**
   * By run id: a thread's own title from plxd (0041), or else its run's (`titleOf`). Runs with no
   * thread, such as a Project's, have their run's.
   */
  titles: Readonly<Record<string, string>>;
  /** By run id: each run, kept current by its repo's or Project's own events. */
  runs: Readonly<Record<string, AgentRun>>;
  /** By run id: the permission requests each run waits on (0033's "needs you"). */
  approvals: ApprovalsByRun;
}

export const emptyThreads: ThreadsState = {
  projects: [],
  repos: [],
  threads: [],
  titles: {},
  runs: {},
  approvals: {},
};

/** How many permission requests run `runId` waits on. */
export const asksOf = (state: ThreadsState, runId: string) =>
  state.approvals[runId]?.items.length ?? 0;

export type ThreadsAction =
  | { type: "snapshot"; projects: Project[]; repos: Repo[]; threads: Thread[]; runs: AgentRun[] }
  | { type: "runs"; runs: AgentRun[] }
  /** A Project's new coordinator, which no host-level event announces (0024). */
  | { type: "coordinator"; run: AgentRun }
  | { type: "event"; event: ParallaxEvent }
  /** A repo's or a Project's own events: its runs and their permission requests. */
  | { type: "scope"; events: LoggedEvent[] }
  /** Older events from runs' logs, read only for the permission requests still waiting. */
  | { type: "approvals"; events: LoggedEvent[] };

/** Applies a snapshot, runs' titles, or a host-level event. Events are upserts, so a repeat is harmless. */
export function threadsReducer(state: ThreadsState, action: ThreadsAction): ThreadsState {
  switch (action.type) {
    case "snapshot":
      return {
        projects: action.projects,
        repos: action.repos,
        threads: action.threads,
        titles: withThreadTitles(titlesOf(action.runs), action.threads),
        runs: byId(action.runs),
        approvals: {},
      };
    case "runs":
      return {
        ...state,
        titles: withThreadTitles({ ...state.titles, ...titlesOf(action.runs) }, state.threads),
        runs: { ...state.runs, ...byId(action.runs) },
      };
    case "coordinator":
      return {
        ...threadsReducer(state, { type: "runs", runs: [action.run] }),
        projects: state.projects.map((p) =>
          p.id === action.run.project ? { ...p, coordinator: action.run.id } : p,
        ),
      };
    case "scope": {
      let runs = state.runs;
      for (const { event, time } of action.events) {
        if (!("runId" in event)) continue;
        const before = runs[event.runId];
        const run = updateRun(before, event);
        // `updatedAt` marks when it last changed, so a run that stops after the user looked
        // counts as unseen (0033).
        if (run && run !== before) runs = { ...runs, [run.id]: { ...run, updatedAt: time } };
      }
      const started = action.events.flatMap((e) =>
        e.event.kind === "agent.started" && e.event.run ? [e.event.run] : [],
      );
      const approvals = trackApprovals(state.approvals, action.events);
      // Output that changes neither, the bulk of a running agent's events, keeps the state.
      if (runs === state.runs && approvals === state.approvals && !started.length) return state;
      return {
        ...state,
        runs,
        titles: started.length
          ? withThreadTitles({ ...state.titles, ...titlesOf(started) }, state.threads)
          : state.titles,
        approvals,
      };
    }
    case "approvals":
      return { ...state, approvals: trackApprovals(state.approvals, action.events) };
    case "event": {
      const e = action.event;
      switch (e.kind) {
        case "project.created":
        case "project.updated":
          return { ...state, projects: upsert(state.projects, e.project) };
        case "project.deleted":
          return {
            ...state,
            projects: state.projects.filter((p) => p.id !== e.project),
            runs: Object.fromEntries(
              Object.entries(state.runs).filter(([, r]) => r.project !== e.project),
            ),
          };
        case "repo.added":
        case "repo.updated":
          return { ...state, repos: upsert(state.repos, e.repo) };
        case "thread.started":
        case "thread.updated": {
          const run = state.runs[e.thread.id];
          // A cleared title falls back to its run's.
          const title = e.thread.title ?? (run && titleOf(run));
          return {
            ...state,
            threads: upsert(state.threads, e.thread),
            titles: title ? { ...state.titles, [e.thread.id]: title } : state.titles,
          };
        }
        case "thread.deleted":
          return { ...state, threads: state.threads.filter((t) => t.id !== e.runId) };
        default:
          return state;
      }
    }
  }
}

function upsert<T extends { id: string }>(list: T[], item: T): T[] {
  return list.some((x) => x.id === item.id)
    ? list.map((x) => (x.id === item.id ? item : x))
    : [...list, item];
}

function byId(runs: AgentRun[]): Record<string, AgentRun> {
  return Object.fromEntries(runs.map((r) => [r.id, r]));
}

function titlesOf(runs: AgentRun[]): Record<string, string> {
  return Object.fromEntries(runs.map((r) => [r.id, titleOf(r)]));
}

/** `titles` with each of `threads`' own title (0041) over its run's. */
function withThreadTitles(titles: Record<string, string>, threads: Thread[]) {
  for (const t of threads) if (t.title) titles[t.id] = t.title;
  return titles;
}

/**
 * A run's title: the generated title this app kept for it, or else its prompt's first line, or
 * "Image" for a prompt of images alone. A thread's own title from plxd comes first (`titles`).
 */
export function titleOf(run: AgentRun): string {
  return readTitle(run.id) ?? (run.prompt.trim().split("\n")[0] || "Image");
}

// A thread's generated title, kept in this app for a plxd without `threadLineage`, which keeps no
// title. A lineage host gets it with `thread/start`, and `useThreads` moves any kept here to it.
// Run ids are unique across hosts. ponytail: not shared with other computers running the app, or
// with a cleared browser profile; both fall back to the prompt's first line.
const titleKey = (runId: string) => `parallax:title:${runId}`;

function readTitle(runId: string): string | undefined {
  try {
    return localStorage.getItem(titleKey(runId)) ?? undefined;
  } catch {
    return undefined;
  }
}

function saveTitle(runId: string, title: string) {
  try {
    localStorage.setItem(titleKey(runId), title);
  } catch {
    // Storage is off: the thread keeps its prompt as its title.
  }
}

function forgetTitle(runId: string) {
  try {
    localStorage.removeItem(titleKey(runId));
  } catch {
    // Storage is off, so nothing was kept.
  }
}

/** The sidebar's id for "No Repo", which holds plxd's scratch entry's threads, made on first use. */
export const noRepo = "no-repo";

export interface ThreadGroup {
  /** A repo entry's id, or `noRepo`. */
  id: string;
  name: string;
  /** Unarchived, newest first. */
  threads: Thread[];
}

/**
 * The sidebar's groups: each repository, oldest first, then No Repo, always there. A thread whose
 * entry isn't known yet goes under No Repo: `thread/start` with no repo answers before plxd's
 * `repo.added` for the scratch entry arrives.
 */
export function groupThreads({ repos, threads }: ThreadsState): {
  groups: ThreadGroup[];
  archived: Thread[];
} {
  const groups: ThreadGroup[] = [
    ...repos.filter((r) => !r.scratch).map((r) => ({ id: r.id, name: r.name, threads: [] })),
    { id: noRepo, name: "No Repo", threads: [] },
  ];
  const newestFirst = [...threads].sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  for (const t of newestFirst.filter((t) => !t.archived))
    (groups.find((g) => g.id === t.repo) ?? groups.at(-1)!).threads.push(t);
  return { groups, archived: newestFirst.filter((t) => t.archived) };
}

/** A thread's group id: its repository's, or `noRepo`. */
export const groupOf = (state: ThreadsState, thread: Thread) =>
  state.repos.some((r) => r.id === thread.repo && !r.scratch) ? thread.repo : noRepo;

/** A thread's parent thread (0041), if it's listed. A Project coordinator's run is no thread. */
export const parentOf = (state: ThreadsState, thread: Thread) =>
  thread.parent === undefined ? undefined : state.threads.find((t) => t.id === thread.parent);

/** The threads `id` launched, oldest first, so their order holds as they start. */
export const childrenOf = (state: ThreadsState, id: string) =>
  state.threads
    .filter((t) => t.parent === id)
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt));

/**
 * What the top bar shows of a thread's lineage: with children, the thread and them; else, as a
 * child, its parent and its unarchived siblings, itself included. Undefined for neither.
 * `parent` is the crumb before the chips.
 */
export function lineageOf(
  state: ThreadsState,
  thread: Thread,
): { parent?: Thread; chips: Thread[]; active?: string } | undefined {
  const parent = parentOf(state, thread);
  const children = childrenOf(state, thread.id).filter((t) => !t.archived);
  if (children.length > 0) return { parent, chips: children };
  if (!parent) return undefined;
  const chips = childrenOf(state, parent.id).filter((t) => !t.archived || t.id === thread.id);
  return { parent, chips, active: thread.id };
}

/** A thread's topmost listed ancestor, for the whole tree. A loop of parents stops where it repeats. */
export function rootOf(state: ThreadsState, thread: Thread): Thread {
  const seen = new Set([thread.id]);
  let root = thread;
  for (let up = parentOf(state, root); up && !seen.has(up.id); up = parentOf(state, up)) {
    seen.add(up.id);
    root = up;
  }
  return root;
}

export interface ThreadsView {
  state: ThreadsState;
  /** Why the list couldn't load or stopped updating, for people. */
  error?: string;
  /** Registers a repository (idempotent on its path). Resolves to its entry or an error message. */
  addRepo: (path: string) => Promise<Repo | string>;
  /**
   * Starts a thread in a group with `prompt` and its `images`, with `options` sent as they are, and
   * its branch and title from `name`. With `checkout`, it works in the repository's own checkout
   * instead of a new worktree, so it gets no branch. `gitRef` is the ref the worktree starts from,
   * or with `checkout`, the branch the checkout switches to first. Reuse `runId`, with the same
   * options, `checkout`, and `gitRef`, to retry. Resolves to plxd's error, or undefined.
   */
  start: (
    runId: string,
    groupId: string,
    prompt: string,
    images: PromptImage[],
    options: RunOptions,
    checkout: boolean,
    gitRef: string | undefined,
    name?: ThreadName,
  ) => Promise<RpcError | undefined>;
  archive: (runId: string, archived: boolean) => Promise<string | undefined>;
  remove: (thread: Thread) => Promise<string | undefined>;
  /**
   * Creates a project on a repository's path, with `icon` if one was chosen. Reuse `id`, with the
   * same name, path, and icon, to retry. Resolves to the project or an error message.
   */
  createProject: (
    id: string,
    name: string,
    repoPath: string,
    icon?: ProjectIconValue,
  ) => Promise<Project | string>;
  /**
   * Renames a project or sets its icon, which replaces the whole icon (0032). Resolves to an error
   * message, or undefined.
   */
  updateProject: (project: string, change: ProjectChange) => Promise<string | undefined>;
  /**
   * Deletes a Project with every run in it, once plxd has stopped them. Resolves to an error
   * message, or undefined.
   */
  removeProject: (project: string) => Promise<string | undefined>;
  /**
   * Starts a Project's coordinator with `prompt` and its `images`, or starts it over with a new
   * `runId` (0024), then keeps it as the Project's. Reusing `runId` to retry is safe with any
   * prompt or options: a failed `project/start` creates nothing, and one whose answer was lost
   * shows up in `project/list` after a reconnect. Resolves to plxd's error, or undefined.
   */
  startCoordinator: (
    project: string,
    runId: string,
    prompt: string,
    images: PromptImage[],
    options: CoordinatorOptions,
  ) => Promise<RpcError | undefined>;
  /** Whether the host's plxd keeps seen and snooze state and repo icons (`threadAttention`, 0033). */
  attention: boolean;
  /** Whether the host's plxd renames Projects and sets their icons (`projectEdit`, 0032). */
  editable: boolean;
  /** Whether the host's plxd deletes Projects (`projectDelete`, PLX-338). */
  deletable: boolean;
  /** The cap on an icon image's base64, where the host's plxd keeps icon images (`iconImages`, 0038). */
  iconImageBytes?: number;
  /** Whether the host's plxd keeps threads' parents and titles (`threadLineage`, 0041). */
  lineage: boolean;
  /**
   * Marks a thread seen, or snoozes it until a time (a past one ends the snooze). Resolves to an
   * error message, or undefined.
   */
  update: (runId: string, change: ThreadChange) => Promise<string | undefined>;
  /** Sets a repo entry's icon. Resolves to an error message, or undefined. */
  updateRepo: (repo: string, icon: ProjectIconValue) => Promise<string | undefined>;
}

/** What `thread/update` changes. */
export type ThreadChange = { seen?: boolean; snoozedUntil?: string };

/** What `project/update` changes: a project's name, its icon, or both. */
export type ProjectChange = Omit<ProjectUpdateParams, "project">;

/** What a new coordinator runs on: its model, effort, permission, and account (`project/start`'s). */
export type CoordinatorOptions = Pick<
  ProjectStartParams,
  "model" | "effort" | "permission" | "account"
>;

/**
 * A host's threads and projects, kept live: `thread/list`, `agent/list` (for titles and runs), and
 * `project/list`, then host-level events after the thread list's `seq`, and each repo's and
 * Project's own events for its runs and their permission requests (0033), starting over on
 * `resync`. Loads only while `connected`. The flags are what the host's plxd advertises: with
 * `approvals`, the threads and coordinators started here forward their permission requests
 * (RYA-196, 0031); with `lineage`, a thread's generated title goes to plxd (0041), and titles kept
 * in this app move there once; `attention`, `editable`, `deletable`, `iconImageBytes`, and
 * `lineage` are passed through for the sidebar and top bar.
 */
export function useThreads(
  hostId: string,
  connected: boolean,
  {
    approvals = false,
    attention = false,
    editable = false,
    deletable = false,
    iconImageBytes,
    lineage = false,
  }: Partial<
    Pick<ThreadsView, "attention" | "editable" | "deletable" | "iconImageBytes" | "lineage">
  > & {
    approvals?: boolean;
  } = {},
): ThreadsView {
  const [state, dispatch] = useReducer(threadsReducer, emptyThreads);
  const [error, setError] = useState<string>();
  // Another host starts empty, rather than showing this one's threads until its list loads.
  const [shownHost, setShownHost] = useState(hostId);
  if (shownHost !== hostId) {
    setShownHost(hostId);
    dispatch({ type: "snapshot", projects: [], repos: [], threads: [], runs: [] });
    setError(undefined);
  }
  // The host shown now, so an answer from one the user has left is dropped.
  const shown = useRef(hostId);
  useEffect(() => {
    shown.current = hostId;
  }, [hostId]);

  useEffect(() => {
    if (!connected) return;
    let stopped = false;
    let unsubscribe = () => {};

    // Each repo's and Project's own events, for its runs' status and permission requests.
    let scopes = new Map<string, () => void>();
    const watch = (scope: string, after: number, logId: string) => {
      if (scopes.has(scope)) return;
      scopes.set(
        scope,
        window.parallax.subscribe(hostId, { after, project: scope, logId }, (message) => {
          if (stopped) return;
          if (message.type === "resync") return void load();
          if (message.type === "event") dispatch({ type: "scope", events: [message.event] });
        }),
      );
    };

    // Titles this app kept before plxd kept them go to plxd, once each, for threads it has none for.
    async function moveTitles(threads: Thread[]) {
      for (const t of threads) {
        const title = t.title ? undefined : readTitle(t.id);
        if (!title) continue;
        const answer = await window.parallax.request(hostId, "thread/update", {
          runId: t.id,
          title,
        });
        if (stopped) return;
        if ("error" in answer) continue;
        forgetTitle(t.id);
        dispatch({
          type: "event",
          event: { kind: "thread.updated", thread: answer.result.thread },
        });
      }
    }

    async function load() {
      for (const stop of scopes.values()) stop();
      scopes = new Map();
      const list = await window.parallax.request(hostId, "thread/list", {});
      if (stopped) return;
      if ("error" in list) return setError(list.error.message);
      // After the list, so every thread listed has its run here.
      const runs = await window.parallax.request(hostId, "agent/list", {});
      if (stopped) return;
      if ("error" in runs) return setError(runs.error.message);
      // Also after the list, whose older `seq` the subscription starts from: a project it
      // replays is already here, and applying it again changes nothing.
      const projects = await window.parallax.request(hostId, "project/list", {});
      if (stopped) return;
      if ("error" in projects) return setError(projects.error.message);
      dispatch({
        type: "snapshot",
        ...list.result,
        projects: projects.result.projects,
        runs: runs.result.runs,
      });
      setError(undefined);
      if (lineage) void moveTitles(list.result.threads);
      // Requests from before the list are in the logs of runs that still go. Read first, and
      // only for requests, so they never undo a newer change from the subscriptions below.
      const backlog = await waitingSince(hostId, runs.result.runs, () => stopped);
      if (stopped) return;
      dispatch({ type: "approvals", events: backlog });
      // From the run list's `seq`, which plxd replays from, so no change since is missed.
      const after = runs.result.seq;
      for (const r of list.result.repos) watch(r.id, after, runs.logId);
      for (const p of projects.result.projects) watch(p.id, after, runs.logId);
      unsubscribe();
      const since = { after: list.result.seq, logId: list.logId };
      unsubscribe = window.parallax.subscribe(hostId, since, (message) => {
        if (stopped) return;
        if (message.type === "resync") return void load();
        if (message.type === "error") return setError(message.error.message);
        const { event } = message.event;
        dispatch({ type: "event", event });
        // A new scope's events start after this one, which is newer than its creation.
        if (event.kind === "repo.added") watch(event.repo.id, message.event.seq, list.logId);
        if (event.kind === "project.created")
          watch(event.project.id, message.event.seq, list.logId);
        // Its title is its run's prompt, which the event doesn't carry.
        if (event.kind === "thread.started")
          void window.parallax
            .request(hostId, "agent/list", { project: event.thread.repo })
            .then((answer) => {
              if (!stopped && "result" in answer)
                dispatch({ type: "runs", runs: answer.result.runs });
            });
      });
    }

    void load();
    return () => {
      stopped = true;
      unsubscribe();
      for (const stop of scopes.values()) stop();
    };
  }, [hostId, connected, lineage]);

  // Each applies its own answer at once; the matching event repeats it harmlessly.
  const addRepo = useCallback(
    async (path: string) => {
      // A fresh id is safe to retry with: plxd returns the entry a path already has.
      const answer = await window.parallax.request(hostId, "repo/add", { id: uuidv7(), path });
      if ("error" in answer) return describeError(answer.error);
      dispatch({ type: "event", event: { kind: "repo.added", repo: answer.result.repo } });
      return answer.result.repo;
    },
    [hostId],
  );

  const start = useCallback(
    async (
      runId: string,
      groupId: string,
      prompt: string,
      images: PromptImage[],
      options: RunOptions,
      checkout: boolean,
      gitRef: string | undefined,
      name?: ThreadName,
    ) => {
      const answer = await window.parallax.request(hostId, "thread/start", {
        runId,
        prompt,
        ...(images.length > 0 && { images }),
        ...(groupId !== noRepo && { repo: groupId }),
        ...options,
        // The checkout keeps its own branch, so a name gives it none.
        ...(checkout ? { checkout } : name?.slug && { branchSlug: name.slug }),
        ...(gitRef && (checkout ? { checkoutRef: gitRef } : { base: gitRef })),
        ...(approvals && { approvals }),
        ...(lineage && name?.title && { title: name.title }),
      });
      if ("error" in answer) return answer.error;
      if (!lineage && name?.title) saveTitle(runId, name.title);
      dispatch({ type: "runs", runs: [answer.result.run] });
      dispatch({ type: "event", event: { kind: "thread.started", thread: answer.result.thread } });
      return undefined;
    },
    [hostId, approvals, lineage],
  );

  const archive = useCallback(
    async (runId: string, archived: boolean) => {
      const answer = await window.parallax.request(hostId, "thread/archive", { runId, archived });
      if ("error" in answer) return answer.error.message;
      dispatch({ type: "event", event: { kind: "thread.updated", thread: answer.result.thread } });
      return undefined;
    },
    [hostId],
  );

  const remove = useCallback(
    async (thread: Thread) => {
      const answer = await window.parallax.request(hostId, "thread/delete", { runId: thread.id });
      if ("error" in answer) return answer.error.message;
      dispatch({
        type: "event",
        event: { kind: "thread.deleted", runId: thread.id, repo: thread.repo },
      });
      return undefined;
    },
    [hostId],
  );

  const createProject = useCallback(
    async (id: string, name: string, repoPath: string, icon?: ProjectIconValue) => {
      const answer = await window.parallax.request(hostId, "project/create", {
        id,
        name,
        repoPath,
        ...(icon && { icon }),
      });
      if ("error" in answer) return describeError(answer.error);
      // Not into another host's list, if the user has left this one.
      if (shown.current === hostId)
        dispatch({
          type: "event",
          event: { kind: "project.created", project: answer.result.project },
        });
      return answer.result.project;
    },
    [hostId],
  );

  const updateProject = useCallback(
    async (project: string, change: ProjectChange) => {
      const answer = await window.parallax.request(hostId, "project/update", {
        project,
        ...change,
      });
      if ("error" in answer) return describeError(answer.error);
      if (shown.current === hostId)
        dispatch({
          type: "event",
          event: { kind: "project.updated", project: answer.result.project },
        });
      return undefined;
    },
    [hostId],
  );

  const removeProject = useCallback(
    async (project: string) => {
      const answer = await window.parallax.request(hostId, "project/delete", { project });
      if ("error" in answer) return describeError(answer.error);
      if (shown.current === hostId)
        dispatch({ type: "event", event: { kind: "project.deleted", project } });
      return undefined;
    },
    [hostId],
  );

  const startCoordinator = useCallback(
    async (
      project: string,
      runId: string,
      prompt: string,
      images: PromptImage[],
      options: CoordinatorOptions,
    ) => {
      const answer = await window.parallax.request(hostId, "project/start", {
        project,
        runId,
        prompt,
        ...(images.length > 0 && { images }),
        ...options,
        ...(approvals && { approvals }),
      });
      if ("error" in answer) return answer.error;
      if (shown.current === hostId) dispatch({ type: "coordinator", run: answer.result.run });
      return undefined;
    },
    [hostId, approvals],
  );

  const update = useCallback(
    async (runId: string, change: ThreadChange) => {
      const answer = await window.parallax.request(hostId, "thread/update", { runId, ...change });
      if ("error" in answer) return describeError(answer.error);
      dispatch({ type: "event", event: { kind: "thread.updated", thread: answer.result.thread } });
      return undefined;
    },
    [hostId],
  );

  const updateRepo = useCallback(
    async (repo: string, icon: ProjectIconValue) => {
      const answer = await window.parallax.request(hostId, "repo/update", { repo, icon });
      if ("error" in answer) return describeError(answer.error);
      dispatch({ type: "event", event: { kind: "repo.updated", repo: answer.result.repo } });
      return undefined;
    },
    [hostId],
  );

  // One object per change, so a parent can keep it and compare.
  return useMemo(
    () => ({
      state,
      error,
      attention,
      editable,
      deletable,
      iconImageBytes,
      lineage,
      update,
      updateRepo,
      addRepo,
      start,
      archive,
      remove,
      createProject,
      updateProject,
      removeProject,
      startCoordinator,
    }),
    [
      state,
      error,
      attention,
      editable,
      deletable,
      iconImageBytes,
      lineage,
      update,
      updateRepo,
      addRepo,
      start,
      archive,
      remove,
      createProject,
      updateProject,
      removeProject,
      startCoordinator,
    ],
  );
}

/**
 * The permission-request events in the logs of `runs` that still go and forward requests, read
 * page by page, for whatever subscribes after the list. A page that fails leaves that run's out;
 * new requests still arrive by subscription.
 */
export async function waitingSince(
  hostId: string,
  runs: AgentRun[],
  stopped: () => boolean,
): Promise<LoggedEvent[]> {
  const events: LoggedEvent[] = [];
  for (const run of runs.filter((r) => r.approvals && isRunning(r.status)))
    for (let after = 0, more = true; more;) {
      const page = await window.parallax.request(hostId, "agent/events", { runId: run.id, after });
      if (stopped() || "error" in page) break;
      events.push(...page.result.events);
      after = page.result.events.at(-1)?.seq ?? after;
      more = page.result.more && page.result.events.length > 0;
    }
  return events;
}

const notConnected = "Not connected to this host.";

/** A host's view before its list loads: empty, and every action answers that it isn't connected. */
export const idleThreads: ThreadsView = {
  state: emptyThreads,
  attention: false,
  editable: false,
  deletable: false,
  lineage: false,
  addRepo: async () => notConnected,
  start: async () => ({ code: -32000, message: notConnected }),
  archive: async () => notConnected,
  remove: async () => notConnected,
  createProject: async () => notConnected,
  updateProject: async () => notConnected,
  removeProject: async () => notConnected,
  startCoordinator: async () => ({ code: -32000, message: notConnected }),
  update: async () => notConnected,
  updateRepo: async () => notConnected,
};
