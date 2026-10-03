import {
  ArrowDownUp,
  ArrowUpRight,
  BookOpen,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleDot,
  CircleMinus,
  CircleX,
  Copy,
  Ellipsis,
  ExternalLink,
  FileDiff,
  GitCommitHorizontal,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  Hammer,
  Link,
  MessageCircleQuestionMark,
  MessageSquare,
  RefreshCw,
  Tag,
  Users,
  type LucideIcon,
} from "lucide-react";
import { Fragment, useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";

import type { RpcError } from "../preload/bridge";
import type {
  PrAction,
  PrCheck,
  PrComment,
  PrDiffResult,
  PullRequest,
} from "../protocol/generated/protocol";
import { MarkdownText } from "./AgentChat";
import { diffBand, diffLook } from "./Approval";
import { tabItem } from "./Composer";
import { describeError, githubProblem } from "./errors";
import { age } from "./Sidebar";
import { menuItem, menuPanel, moveFocus, SetUpGithub } from "./ui";

/** A linked pull request as last read: GitHub's view of it, when, and why a read or action failed. */
export interface Linked {
  pr?: PullRequest;
  at?: string;
  error?: RpcError;
}

/** A thread's linked pull requests (PLX-318), as `usePullRequests` keeps them. */
export interface PullRequests {
  /** Their URLs, oldest first. */
  urls: readonly string[];
  get: (url: string) => Linked | undefined;
  /** Reads one again. */
  refresh: (url: string) => Promise<void>;
  /** Runs `action` on one, keeping what GitHub has after it. Resolves to its error, if it failed. */
  act: (url: string, action: PrAction) => Promise<RpcError | undefined>;
  /** Reads one's unified diff, on a plxd with `prDiff` (PLX-328); absent on an older one. */
  diff?: (url: string) => Promise<{ result?: PrDiffResult; error?: string }>;
}

/**
 * Reads each of a run's linked pull requests with `pr/view`, again whenever the list changes, and
 * acts on them with `pr/act`. With `diffs`, reads one's diff with `pr/diff` on demand. Reads are kept by run and URL, so a late answer for another thread
 * lands under that thread. ponytail: every linked one is read on open; read on demand if threads
 * come to link many.
 */
export function usePullRequests(
  hostId: string,
  runId: string | undefined,
  urls: readonly string[],
  diffs = false,
): PullRequests {
  const [read, setRead] = useState<Readonly<Record<string, Linked>>>({});
  const put = useCallback(
    (url: string, next: (prev?: Linked) => Linked) =>
      setRead((all) => ({ ...all, [`${runId} ${url}`]: next(all[`${runId} ${url}`]) })),
    [runId],
  );
  const refresh = useCallback(
    async (url: string) => {
      if (!runId) return;
      const answer = await window.parallax.request(hostId, "pr/view", { runId, url });
      const at = new Date().toISOString();
      if ("error" in answer) put(url, (prev) => ({ ...prev, error: answer.error }));
      else put(url, () => ({ pr: answer.result, at }));
    },
    [hostId, runId, put],
  );
  // Kept across renders, so the Code tab's read isn't restarted by every App render.
  const diff = useCallback(
    async (url: string) => {
      const answer = await window.parallax.request(hostId, "pr/diff", { runId: runId!, url });
      return "error" in answer ? { error: describeError(answer.error) } : { result: answer.result };
    },
    [hostId, runId],
  );
  const key = urls.join("\n");
  useEffect(() => {
    for (const url of key ? key.split("\n") : []) void refresh(url);
  }, [key, refresh]);
  return {
    urls,
    get: (url) => read[`${runId} ${url}`],
    refresh,
    act: async (url, action) => {
      if (!runId) return;
      const answer = await window.parallax.request(hostId, "pr/act", { runId, url, action });
      if ("error" in answer) return answer.error;
      put(url, () => ({ pr: answer.result, at: new Date().toISOString() }));
    },
    diff: diffs && runId ? diff : undefined,
  };
}

/** A pull request's number, from its URL. */
export const numberOf = (url: string) => /\/pull\/(\d+)/.exec(url)?.[1] ?? "?";
const repoOf = (url: string) => /github\.com\/([^/]+\/[^/]+)\/pull\//.exec(url)?.[1] ?? "";

/** How a pull request's state looks: open green, draft gray, merged purple, closed red. */
function lookOf(pr?: PullRequest): { Icon: LucideIcon; color: string; label?: string } {
  if (pr?.state === "merged")
    return { Icon: GitMerge, color: "text-project-violet", label: "Merged" };
  if (pr?.state === "closed")
    return { Icon: GitPullRequestClosed, color: "text-danger", label: "Closed" };
  if (pr?.state === "open" && pr.draft)
    return { Icon: GitPullRequestDraft, color: "text-faint-foreground", label: "Draft" };
  if (pr?.state === "open") return { Icon: GitPullRequest, color: "text-added", label: "Open" };
  // Not read yet, or a state this version doesn't know.
  return { Icon: GitPullRequest, color: "text-muted-foreground" };
}

const ago = (time: string) => (age(time) === "now" ? "just now" : `${age(time)} ago`);

/**
 * The composer tab's link to a thread's pull requests: the latest one's number, colored by its
 * state, and `+k` for the others. It opens that one's view, or with others, the list.
 */
export function PullRequestChip({
  prs,
  onOpen,
}: {
  prs: PullRequests;
  onOpen: (url?: string) => void;
}) {
  const latest = prs.urls.at(-1);
  if (!latest) return null;
  const pr = prs.get(latest)?.pr;
  const { Icon, color } = lookOf(pr);
  const more = prs.urls.length - 1;
  return (
    <button
      type="button"
      title={pr ? `#${pr.number} ${pr.title}` : latest}
      onClick={() => onOpen(more > 0 ? undefined : latest)}
      className={`${tabItem} shrink-0 rounded-md hover:bg-hover`}
    >
      <span className={`flex items-center gap-1 ${color}`}>
        <Icon aria-hidden />#{numberOf(latest)}
      </span>
      {more > 0 && <span>+{more}</span>}
    </button>
  );
}

/**
 * The side panel's Pull requests view: the thread's linked pull requests, newest first, each
 * opening its own view, over a footer counting the open and linked ones and when they were read.
 */
export function PullRequestList({
  prs,
  onOpen,
}: {
  prs: PullRequests;
  onOpen: (url: string) => void;
}) {
  const read = prs.urls.map((url) => prs.get(url));
  const open = read.filter((r) => r?.pr?.state === "open").length;
  // The oldest read, since the list is only as current as it.
  const synced = read.every((r) => r?.at) ? read.map((r) => r!.at!).sort()[0] : undefined;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ul aria-label="Pull requests" className="min-h-0 flex-1 overflow-y-auto p-2">
        {[...prs.urls].reverse().map((url) => {
          const { pr, error } = prs.get(url) ?? {};
          const { Icon, color } = lookOf(pr);
          return (
            <li key={url}>
              <button
                type="button"
                onClick={() => onOpen(url)}
                className="flex w-full gap-2.5 rounded-lg px-2 py-2 text-left hover:bg-hover"
              >
                <Icon aria-hidden className={`mt-0.5 size-4 shrink-0 ${color}`} />
                <span className="min-w-0 flex-1">
                  <span className="flex items-baseline gap-1.5 text-[13.5px]">
                    <span className="shrink-0 text-faint-foreground">#{numberOf(url)}</span>
                    <span className="min-w-0 flex-1 truncate">{pr?.title ?? repoOf(url)}</span>
                    {pr && (
                      <span className="shrink-0 font-mono text-[12px]">
                        <span className="text-added">+{pr.additions.toLocaleString()}</span>{" "}
                        <span className="text-danger">−{pr.deletions.toLocaleString()}</span>
                      </span>
                    )}
                  </span>
                  <span className="mt-0.5 flex items-baseline gap-2 text-[12px] text-faint-foreground">
                    {pr ? (
                      <>
                        <span className="shrink-0">{pr.author}</span>
                        <span className="min-w-0 truncate">{pr.repo}</span>
                        <span className="min-w-0 flex-1 truncate font-mono">
                          {pr.headBranch} → {pr.baseBranch}
                        </span>
                        <span className="shrink-0">{ago(pr.updatedAt)}</span>
                      </>
                    ) : (
                      <span className={error ? "text-danger" : ""}>
                        {error ? describeError(error) : "Loading…"}
                      </span>
                    )}
                  </span>
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      <p className="border-t border-border px-4 py-2.5 text-[12.5px] text-faint-foreground">
        {open} open · {prs.urls.length} linked · {synced ? `synced ${ago(synced)}` : "syncing…"}
      </p>
    </div>
  );
}

/** All of a pull request's checks in a few words, as its Summary tab shows them. */
function checksOf(pr: PullRequest): { Icon: LucideIcon; color: string; text: string } {
  const total = pr.checks.length;
  const count = (state: PrCheck["state"]) => pr.checks.filter((c) => c.state === state).length;
  switch (pr.checksState) {
    case "failed":
      return { Icon: CircleX, color: "text-danger", text: `${count("failed")} of ${total} failed` };
    case "pending":
      return {
        Icon: CircleDot,
        color: "text-warning",
        text: `${count("pending")} of ${total} running`,
      };
    case "passed":
      return { Icon: CircleCheck, color: "text-added", text: "All checks passed" };
    default:
      return {
        Icon: CircleMinus,
        color: "text-faint-foreground",
        text: total ? "Checks skipped" : "No checks",
      };
  }
}

const checkIcons: Record<string, { Icon: LucideIcon; color: string }> = {
  passed: { Icon: CircleCheck, color: "text-added" },
  failed: { Icon: CircleX, color: "text-danger" },
  pending: { Icon: CircleDot, color: "text-warning" },
};

/** A menu row: its icon, label, and an optional line under it. */
function Item({
  Icon,
  label,
  hint,
  danger,
  disabled,
  onClick,
}: {
  Icon: LucideIcon;
  label: string;
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      onClick={onClick}
      className={`${menuItem} ${hint ? "items-start" : ""} ${danger ? "text-danger" : ""} disabled:opacity-50 disabled:hover:bg-transparent [&_svg]:size-4`}
    >
      <span className="grid h-5 place-items-center">
        <Icon aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block leading-5">{label}</span>
        {hint && <span className="block text-[12px] text-faint-foreground">{hint}</span>}
      </span>
    </button>
  );
}

const separator = <div role="separator" className="-mx-1 my-1 h-px bg-border" />;

/** A section of the Summary tab whose heading folds it. */
function Section({
  title,
  open,
  onToggle,
  aside,
  children,
}: {
  title: string;
  open: boolean;
  onToggle: () => void;
  aside?: ReactNode;
  children: ReactNode;
}) {
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <section className="mt-4">
      <div className="flex items-center">
        <button
          type="button"
          aria-expanded={open}
          onClick={onToggle}
          className="flex items-center gap-1 rounded-md py-0.5 text-[13px] text-muted-foreground hover:text-foreground"
        >
          {title}
          <Chevron aria-hidden className="size-3.5" />
        </button>
        {aside}
      </div>
      {open && <div className="mt-2">{children}</div>}
    </section>
  );
}

// ponytail: "long" by length, not rendered height; measure it if this cuts the wrong ones.
const isLong = (c: PrComment) => c.body.length > 600 || c.body.split("\n").length > 12;

/** A comment's card: its author, age, and Markdown, folded when long until `onUnfold`. */
function CommentCard({
  c,
  folded,
  onUnfold,
}: {
  c: PrComment;
  folded: boolean;
  onUnfold: () => void;
}) {
  return (
    <div className="rounded-xl border border-border px-4 py-3">
      <p className="text-[13px]">
        <span className="font-medium">{c.author}</span>{" "}
        <span className="text-faint-foreground">{ago(c.createdAt)}</span>
      </p>
      <div
        className={`mt-2 select-text ${folded ? "max-h-48 overflow-hidden [mask-image:linear-gradient(to_bottom,black_55%,transparent)]" : ""}`}
      >
        <MarkdownText text={c.body} />
      </div>
      {folded && (
        <button
          type="button"
          onClick={onUnfold}
          className="mt-2 text-[13px] text-muted-foreground hover:text-foreground"
        >
          Show full comment
        </button>
      )}
    </div>
  );
}

/** A line of a diff: added, removed, context, or a hunk's `@@` header, with its line numbers. */
export interface DiffLine {
  op: "+" | "-" | " " | "@";
  text: string;
  old?: number;
  new?: number;
}

/** One file of a unified diff. */
export interface DiffFile {
  path: string;
  /** Its path before a rename. */
  oldPath?: string;
  added: number;
  removed: number;
  binary: boolean;
  lines: DiffLine[];
}

/** `gh pr diff`'s unified diff as its files, each with its lines numbered on both sides. */
export function parseDiff(diff: string): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | undefined;
  let old = 0;
  let now = 0;
  for (const line of diff.split("\n")) {
    if (line.startsWith("diff --git ")) {
      // `a/<path> b/<path>`: the same path twice unless renamed. A `rename to` or `+++` line
      // below, when there is one, says it exactly.
      const rest = line.slice(11).replaceAll('"', "");
      const half = (rest.length - 1) / 2;
      const same = rest.slice(2, half) === rest.slice(half + 3);
      file = {
        path: same ? rest.slice(2, half) : (/ b\/(.*)$/.exec(rest)?.[1] ?? rest),
        added: 0,
        removed: 0,
        binary: false,
        lines: [],
      };
      files.push(file);
    } else if (!file) {
      continue;
    } else if (line.startsWith("@@")) {
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(line);
      old = Number(m?.[1] ?? 0);
      now = Number(m?.[2] ?? 0);
      file.lines.push({ op: "@", text: line });
    } else if (file.lines.length === 0) {
      // The file's header, before its first hunk.
      // git quotes a path with special characters, and escapes them.
      if (line.startsWith("rename from ")) file.oldPath = line.slice(12).replaceAll('"', "");
      else if (line.startsWith("rename to ")) file.path = line.slice(10).replaceAll('"', "");
      else if (line.startsWith("+++ b/")) file.path = line.slice(6);
      else if (line.startsWith('+++ "b/')) file.path = line.slice(7, -1);
      else if (line.startsWith("Binary files ")) file.binary = true;
    } else if (line.startsWith("+")) {
      file.added++;
      file.lines.push({ op: "+", text: line.slice(1), new: now++ });
    } else if (line.startsWith("-")) {
      file.removed++;
      file.lines.push({ op: "-", text: line.slice(1), old: old++ });
    } else if (line.startsWith(" ")) {
      file.lines.push({ op: " ", text: line.slice(1), old: old++, new: now++ });
    }
  }
  return files;
}

/**
 * One file of the Code tab: a header that folds it, with its path, size, and Viewed, over its
 * lines numbered on both sides. ponytail: every line renders; window them if huge diffs lag.
 */
function DiffFileView({
  file,
  viewed,
  onViewed,
}: {
  file: DiffFile;
  viewed: boolean;
  onViewed: (viewed: boolean) => void;
}) {
  const [folded, setFolded] = useState(false);
  const shut = folded || viewed;
  const Chevron = shut ? ChevronRight : ChevronDown;
  return (
    <li className="border-b border-border">
      <div
        className={`flex items-center gap-2 bg-code px-3 py-2 text-[13px] ${shut ? "" : "border-b border-border"}`}
      >
        <button
          type="button"
          aria-expanded={!shut}
          aria-label={`${shut ? "Show" : "Hide"} ${file.path}`}
          onClick={() => (viewed ? onViewed(false) : setFolded(!folded))}
          className="grid size-5 shrink-0 place-items-center rounded text-muted-foreground hover:bg-hover"
        >
          <Chevron aria-hidden className="size-4" />
        </button>
        <span
          className="min-w-0 flex-1 truncate font-mono text-[12.5px]"
          title={file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}
        >
          {file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}
        </span>
        <span className="shrink-0 font-mono text-[12px]">
          <span className="text-added">+{file.added}</span>{" "}
          <span className="text-danger">−{file.removed}</span>
        </span>
        <label className="flex shrink-0 items-center gap-1.5 text-[12.5px] text-muted-foreground">
          <input type="checkbox" checked={viewed} onChange={(e) => onViewed(e.target.checked)} />
          Viewed
        </label>
      </div>
      {!shut && (
        <div className="code-scroll pb-1 font-mono text-[12px] leading-5">
          {file.binary && <p className="px-3 text-faint-foreground">Binary file not shown</p>}
          {file.lines.map((line, i) =>
            line.op === "@" ? (
              <div key={i} className={`${diffBand} code-lines break-all whitespace-pre-wrap`}>
                {line.text}
              </div>
            ) : (
              <div key={i} className={`flex ${diffLook[line.op].row}`}>
                <span
                  aria-hidden
                  className="w-10 shrink-0 pr-2 text-right text-faint-foreground/70 select-none"
                >
                  {line.old}
                </span>
                <span
                  aria-hidden
                  className="w-10 shrink-0 pr-2 text-right text-faint-foreground/70 select-none"
                >
                  {line.new}
                </span>
                <span aria-hidden className={`w-5 shrink-0 select-none ${diffLook[line.op].sign}`}>
                  {line.op === "-" ? "−" : line.op}
                </span>
                {line.op !== " " && (
                  <span className="sr-only">{line.op === "+" ? "Added: " : "Removed: "}</span>
                )}
                <span className="code-lines min-w-0 pr-3 break-all whitespace-pre-wrap">
                  {line.text || " "}
                </span>
              </div>
            ),
          )}
        </div>
      )}
    </li>
  );
}

/**
 * A review verdict in words, and its icon. A plain "commented" review shows as its comment, or,
 * with only inline comments, as "reviewed".
 */
const verdicts: Record<string, { Icon: LucideIcon; color: string; text: string }> = {
  commented: { Icon: MessageSquare, color: "text-muted-foreground", text: "reviewed" },
  approved: { Icon: CircleCheck, color: "text-added", text: "approved these changes" },
  changesRequested: { Icon: CircleAlert, color: "text-danger", text: "requested changes" },
  dismissed: { Icon: CircleMinus, color: "text-faint-foreground", text: "had a review dismissed" },
};

/** One row of the Timeline: its icon on the rail, a line of what happened, and what's under it. */
function TimelineEvent({
  Icon,
  color = "text-muted-foreground",
  title,
  sub,
  children,
}: {
  Icon: LucideIcon;
  color?: string;
  title: ReactNode;
  sub?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <li className="relative flex gap-3 pb-5 before:absolute before:top-7 before:bottom-0 before:left-[11px] before:w-px before:bg-border last:before:hidden">
      <span className="grid size-6 shrink-0 place-items-center rounded-full bg-surface">
        <Icon aria-hidden className={`size-4 ${color}`} />
      </span>
      <div className="min-w-0 flex-1 pt-0.5">
        <p className="text-[13px]">{title}</p>
        {sub && <p className="mt-0.5 text-[12px] text-faint-foreground">{sub}</p>}
        {children && <div className="mt-2">{children}</div>}
      </div>
    </li>
  );
}

/**
 * One linked pull request in the side panel: its repository and number (a link to GitHub), comment
 * count, Merge, and a `…` menu of everything else; its title, author, branches, and size; then its
 * tabs (PLX-328): a Summary of its reviewers, labels, description, and comments, newest first; a
 * Timeline of its commits, comments, reviews, and merge; and, on a plxd with `prDiff`, the Code it
 * changes. The checks' state beside the tabs opens a list of every check.
 * `onCompose` hands a message to this thread's chat: sent, or put in the composer to finish.
 */
export function PullRequestView({
  url,
  prs,
  onCompose,
  onSetUpGithub,
}: {
  url: string;
  prs: PullRequests;
  onCompose: (text: string, send: boolean) => void;
  /** Offered when a read or action fails because `gh` is missing or signed out (PLX-423). */
  onSetUpGithub?: () => void;
}) {
  const id = useId();
  const menu = useRef<HTMLDivElement>(null);
  const mergeMenu = useRef<HTMLDivElement>(null);
  const closeDialog = useRef<HTMLDialogElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<RpcError>();
  const [tab, setTab] = useState<"summary" | "timeline" | "code">("summary");
  const [description, setDescription] = useState(true);
  const [comments, setComments] = useState(true);
  const [full, setFull] = useState<ReadonlySet<number>>(new Set());
  const [oldestFirst, setOldestFirst] = useState(false);
  // The diff as read for the pull request's `updatedAt`, and the files marked viewed, by path.
  const [diff, setDiff] = useState<{ for: string; result?: PrDiffResult; error?: string }>();
  const [viewed, setViewed] = useState<ReadonlySet<string>>(new Set());
  const linked = prs.get(url);
  const pr = linked?.pr;
  const readDiff = prs.diff;
  const updatedAt = pr?.updatedAt;
  useEffect(() => {
    if (tab !== "code" || !readDiff || !updatedAt || diff?.for === updatedAt) return;
    let live = true;
    void readDiff(url).then((read) => live && setDiff({ for: updatedAt, ...read }));
    return () => {
      live = false;
    };
  }, [tab, readDiff, url, updatedAt, diff?.for]);

  // Runs `fn` from the menus, which close first; its error shows over the Summary.
  const run = async (fn: () => Promise<RpcError | undefined | void>) => {
    menu.current?.hidePopover();
    mergeMenu.current?.hidePopover();
    setBusy(true);
    setError(undefined);
    setError((await fn()) ?? undefined);
    setBusy(false);
  };
  const act = (action: PrAction) => void run(() => prs.act(url, action));
  const refresh = () => void run(() => prs.refresh(url));
  const compose = (text: string, send: boolean) => {
    menu.current?.hidePopover();
    onCompose(text, send);
  };
  const copy = (text: string) => {
    menu.current?.hidePopover();
    void navigator.clipboard.writeText(text);
  };
  const failed = error ?? linked?.error;
  // `gh` missing or signed out: a short line by Set up GitHub, in place of plxd's message.
  const github = onSetUpGithub && githubProblem(failed);
  const failure = failed && (github || describeError(failed));
  const setUp = github && <SetUpGithub onClick={onSetUpGithub!} />;

  if (!pr)
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 px-8 pb-16 text-center text-[13px]">
        {failure ? (
          <>
            <p role="alert" className="text-danger">
              {failure}
            </p>
            {setUp}
            <button
              type="button"
              disabled={busy}
              onClick={refresh}
              className="rounded-md px-2.5 py-1 text-muted-foreground hover:bg-hover hover:text-foreground"
            >
              Try again
            </button>
          </>
        ) : (
          <p className="text-faint-foreground">Loading…</p>
        )}
      </div>
    );

  const open = pr.state === "open";
  const look = lookOf(pr);
  const summary = checksOf(pr);
  const newest = pr.comments.map((c, i) => ({ c, i })).reverse();
  const card = (c: PrComment, i: number) => (
    <CommentCard
      c={c}
      folded={isLong(c) && !full.has(i)}
      onUnfold={() => setFull(new Set(full).add(i))}
    />
  );
  const files = diff?.result ? parseDiff(diff.result.diff) : [];
  // Every event, by time; a plain "commented" review shows as its comment instead.
  const events: { at: string; key: string; node: ReactNode }[] = [
    ...(pr.createdAt
      ? [
          {
            at: pr.createdAt,
            key: "opened",
            node: (
              <TimelineEvent
                Icon={GitPullRequest}
                color="text-added"
                title={
                  <>
                    <span className="font-medium">{pr.author}</span> opened this pull request
                  </>
                }
                sub={ago(pr.createdAt)}
              />
            ),
          },
        ]
      : []),
    ...(pr.commits ?? []).map((c) => ({
      at: c.committedAt,
      key: `commit ${c.oid}`,
      node: (
        <TimelineEvent
          Icon={GitCommitHorizontal}
          title={<span className="font-medium">{c.headline}</span>}
          sub={
            <>
              <span className="font-mono">{c.oid.slice(0, 7)}</span> · {c.author} ·{" "}
              {ago(c.committedAt)}
            </>
          }
        />
      ),
    })),
    ...pr.comments.map((c, i) => ({
      at: c.createdAt,
      key: `comment ${i}`,
      node: (
        <TimelineEvent
          Icon={MessageSquare}
          title={
            <>
              <span className="font-medium">{c.author}</span> commented
            </>
          }
        >
          {card(c, i)}
        </TimelineEvent>
      ),
    })),
    ...(pr.reviews ?? []).flatMap((r, i) => {
      const verdict = verdicts[r.state];
      const said = pr.comments.some((c) => c.author === r.author && c.createdAt === r.submittedAt);
      if (!verdict || (r.state === "commented" && said)) return [];
      return [
        {
          at: r.submittedAt,
          key: `review ${i}`,
          node: (
            <TimelineEvent
              Icon={verdict.Icon}
              color={verdict.color}
              title={
                <>
                  <span className="font-medium">{r.author}</span> {verdict.text}
                </>
              }
              sub={ago(r.submittedAt)}
            />
          ),
        },
      ];
    }),
    ...(pr.mergedAt
      ? [
          {
            at: pr.mergedAt,
            key: "merged",
            node: (
              <TimelineEvent
                Icon={GitMerge}
                color="text-project-violet"
                title={
                  <>
                    <span className="font-medium">{pr.mergedBy ?? "Someone"}</span> merged{" "}
                    <span className="font-mono">{pr.headBranch}</span> into{" "}
                    <span className="font-mono">{pr.baseBranch}</span>
                  </>
                }
                sub={ago(pr.mergedAt)}
              />
            ),
          },
        ]
      : pr.state === "closed" && pr.closedAt
        ? [
            {
              at: pr.closedAt,
              key: "closed",
              node: (
                <TimelineEvent
                  Icon={GitPullRequestClosed}
                  color="text-danger"
                  title="Closed without merging"
                  sub={ago(pr.closedAt)}
                />
              ),
            },
          ]
        : []),
  ].sort((a, b) => a.at.localeCompare(b.at));
  if (!oldestFirst) events.reverse();
  const tabButton = (name: typeof tab, label: string) => (
    <button
      type="button"
      role="tab"
      aria-selected={tab === name}
      onClick={() => setTab(name)}
      className={`rounded-lg px-2.5 py-1 text-[13px] ${tab === name ? "bg-selected font-medium" : "text-muted-foreground hover:text-foreground"}`}
    >
      {label}
    </button>
  );
  const sort = (
    <button
      type="button"
      onClick={() => setOldestFirst(!oldestFirst)}
      className="ml-auto flex items-center gap-1 text-[12.5px] text-muted-foreground hover:text-foreground"
    >
      <ArrowDownUp aria-hidden className="size-3.5" />
      {oldestFirst ? "Oldest first" : "Newest first"}
    </button>
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <header className="border-b border-border px-4 pt-2 pb-3">
        <div className="flex items-center gap-2">
          <a
            href={pr.url}
            target="_blank"
            rel="noreferrer"
            title="Open on GitHub"
            className="flex min-w-0 items-center gap-1 text-[13px] text-muted-foreground hover:text-foreground"
          >
            <span className="truncate">{pr.repo}</span>
            <span className={`shrink-0 ${look.color}`}>#{pr.number}</span>
            <ExternalLink aria-hidden className={`size-3.5 shrink-0 ${look.color}`} />
          </a>
          <span
            title={`${pr.comments.length} comments`}
            className="ml-auto flex shrink-0 items-center gap-1 text-[13px] text-muted-foreground"
          >
            <MessageSquare aria-hidden className="size-4" />
            {pr.comments.length}
          </span>
          {open ? (
            <div className="flex shrink-0 items-center rounded-lg bg-accent text-[13px] font-medium text-accent-foreground">
              <button
                type="button"
                disabled={busy}
                title={pr.autoMerge ? "Disable auto-merge" : "Merge this pull request"}
                onClick={() => act(pr.autoMerge ? "disableAutoMerge" : "merge")}
                className="flex items-center gap-1.5 rounded-l-lg py-1 pr-2 pl-2.5 enabled:hover:bg-black/10 disabled:opacity-60"
              >
                <GitMerge aria-hidden className="size-4" />
                {pr.autoMerge ? `Auto-merge (${pr.autoMerge})` : "Merge"}
              </button>
              <button
                type="button"
                popoverTarget={`${id}-merge`}
                aria-haspopup="menu"
                aria-label="Merge options"
                className="self-stretch rounded-r-lg border-l border-black/15 px-1 hover:bg-black/10"
              >
                <ChevronDown aria-hidden className="size-4" />
              </button>
            </div>
          ) : (
            <span className={`shrink-0 text-[13px] ${look.color}`}>{look.label}</span>
          )}
          <button
            type="button"
            popoverTarget={`${id}-menu`}
            aria-haspopup="menu"
            aria-label="Pull request actions"
            title="Pull request actions"
            className="grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-4"
          >
            <Ellipsis />
          </button>
        </div>
        <h2 className="mt-2 truncate text-[15px] font-semibold" title={pr.title}>
          {pr.title}
        </h2>
        <p className="mt-1 text-[12.5px] text-muted-foreground">
          {pr.author} <span className="text-faint-foreground">· updated {ago(pr.updatedAt)}</span>
        </p>
        <div className="mt-2 flex items-center gap-3 text-[12.5px]">
          <span
            className="min-w-0 flex-1 truncate font-mono text-faint-foreground"
            title={`${pr.headBranch} into ${pr.baseBranch}`}
          >
            {pr.baseBranch} ← {pr.headBranch}
          </span>
          <span className="flex shrink-0 items-center gap-1.5 text-muted-foreground">
            <FileDiff aria-hidden className="size-4" />
            {pr.changedFiles} {pr.changedFiles === 1 ? "file" : "files"}
            <span className="font-mono text-added">+{pr.additions.toLocaleString()}</span>
            <span className="font-mono text-danger">−{pr.deletions.toLocaleString()}</span>
          </span>
        </div>
      </header>

      <div className="flex items-center gap-1 border-b border-border px-4 py-2">
        <div role="tablist" aria-label="Pull request" className="flex gap-1">
          {tabButton("summary", "Summary")}
          {tabButton("timeline", "Timeline")}
          {prs.diff && tabButton("code", "Code")}
        </div>
        <button
          type="button"
          popoverTarget={`${id}-checks`}
          aria-haspopup="dialog"
          className={`ml-auto flex items-center gap-1.5 rounded-md px-1.5 py-0.5 text-[12.5px] hover:bg-hover ${summary.color}`}
        >
          <summary.Icon aria-hidden className="size-4" />
          <span className="text-muted-foreground">{summary.text}</span>
        </button>
      </div>

      {failure && (
        <p role="alert" className="mx-4 mt-3 flex items-center gap-2 text-[12.5px] text-danger">
          {failure}
          {setUp}
        </p>
      )}

      {tab === "timeline" && (
        <div className="px-4 pb-6">
          <div className="flex items-center gap-1.5 py-3 text-[12.5px] text-muted-foreground">
            <MessageSquare aria-hidden className="size-3.5" />
            {pr.comments.length} · <GitCommitHorizontal aria-hidden className="size-3.5" />
            {pr.commits?.length ?? 0}
            {sort}
          </div>
          <ol aria-label="Timeline">
            {events.map((e) => (
              <Fragment key={e.key}>{e.node}</Fragment>
            ))}
          </ol>
        </div>
      )}

      {tab === "code" && (
        <div className="pb-6">
          {!diff ? (
            <p className="px-4 py-3 text-[13px] text-faint-foreground">Loading…</p>
          ) : diff.error ? (
            <p role="alert" className="px-4 py-3 text-[13px] text-danger">
              {diff.error}
            </p>
          ) : (
            <>
              <p className="border-b border-border px-4 py-2.5 text-[12.5px] text-muted-foreground">
                {files.length} {files.length === 1 ? "file" : "files"} ·{" "}
                {files.filter((f) => viewed.has(f.path)).length} / {files.length} viewed
              </p>
              {diff.result?.truncated && (
                <p className="border-b border-border px-4 py-2.5 text-[12.5px] text-warning">
                  This diff is too large to show whole. Open it on GitHub for the rest.
                </p>
              )}
              <ul aria-label="Changed files">
                {files.map((f) => (
                  <DiffFileView
                    key={f.path}
                    file={f}
                    viewed={viewed.has(f.path)}
                    onViewed={(on) => {
                      const next = new Set(viewed);
                      if (on) next.add(f.path);
                      else next.delete(f.path);
                      setViewed(next);
                    }}
                  />
                ))}
              </ul>
            </>
          )}
        </div>
      )}

      <div className={tab === "summary" ? "px-4 pb-6" : "hidden"}>
        <dl className="mt-3 grid grid-cols-[7rem_1fr] gap-y-2.5 text-[13px]">
          <dt className="flex items-center gap-2 text-muted-foreground">
            <Users aria-hidden className="size-4" />
            Reviewers
          </dt>
          <dd className={pr.reviewRequests.length ? "" : "text-faint-foreground"}>
            {pr.reviewRequests.join(", ") || "None"}
          </dd>
          <dt className="flex items-center gap-2 text-muted-foreground">
            <Tag aria-hidden className="size-4" />
            Labels
          </dt>
          <dd className="flex flex-wrap gap-1">
            {pr.labels.length ? (
              pr.labels.map((l) => (
                <span key={l} className="rounded-full border border-border px-2 text-[12px]">
                  {l}
                </span>
              ))
            ) : (
              <span className="text-faint-foreground">None</span>
            )}
          </dd>
        </dl>

        <Section
          title="Description"
          open={description}
          onToggle={() => setDescription(!description)}
        >
          {pr.body.trim() ? (
            <MarkdownText text={pr.body} />
          ) : (
            <p className="text-[13px] text-faint-foreground">No description</p>
          )}
        </Section>

        <Section
          title={`Comments (${pr.comments.length})`}
          open={comments}
          onToggle={() => setComments(!comments)}
          aside={
            <span className="ml-auto flex items-center gap-1 text-[12.5px] text-muted-foreground">
              <ArrowDownUp aria-hidden className="size-3.5" />
              Newest first
            </span>
          }
        >
          <ul className="flex flex-col gap-3">
            {newest.map(({ c, i }) => (
              <li key={i}>{card(c, i)}</li>
            ))}
            {pr.comments.length === 0 && (
              <li className="text-[13px] text-faint-foreground">No comments</li>
            )}
          </ul>
        </Section>
      </div>

      <div
        id={`${id}-checks`}
        popover="auto"
        role="dialog"
        aria-label="Checks"
        className={`${menuPanel("end")} w-96 p-3`}
      >
        <p className="text-[14px] font-medium">{summary.text}</p>
        <ul className="mt-2 flex flex-col gap-1.5">
          {pr.checks.map((c, i) => {
            const { Icon, color } = checkIcons[c.state] ?? {
              Icon: CircleMinus,
              color: "text-faint-foreground",
            };
            return (
              <li key={i} className="flex items-center gap-2 text-[13px]">
                <Icon aria-hidden className={`size-4 shrink-0 ${color}`} />
                <span className="min-w-0 flex-1 truncate">{c.name}</span>
                <span className="shrink-0 text-[12px] text-faint-foreground">
                  {c.conclusion ?? c.state}
                </span>
                {c.url && (
                  <a
                    href={c.url}
                    target="_blank"
                    rel="noreferrer"
                    className="shrink-0 text-[12px] text-accent hover:underline"
                  >
                    Details
                  </a>
                )}
              </li>
            );
          })}
          {pr.checks.length === 0 && <li className="text-[13px] text-faint-foreground">None</li>}
        </ul>
      </div>

      {open && (
        <div
          ref={mergeMenu}
          id={`${id}-merge`}
          popover="auto"
          role="menu"
          aria-label="Merge"
          onKeyDown={moveFocus}
          className={`${menuPanel("end")} w-56 p-1`}
        >
          <Item Icon={GitMerge} label="Merge" disabled={busy} onClick={() => act("merge")} />
          <Item
            Icon={GitMerge}
            label="Squash and merge"
            disabled={busy}
            onClick={() => act("squash")}
          />
          {pr.autoMerge ? (
            <Item
              Icon={GitMerge}
              label="Disable auto-merge"
              disabled={busy}
              onClick={() => act("disableAutoMerge")}
            />
          ) : (
            <Item
              Icon={GitMerge}
              label="Enable auto-merge"
              disabled={busy}
              onClick={() => act("autoMerge")}
            />
          )}
        </div>
      )}

      <div
        ref={menu}
        id={`${id}-menu`}
        popover="auto"
        role="menu"
        aria-label="Pull request"
        onKeyDown={moveFocus}
        className={`${menuPanel("end")} w-80 p-1`}
      >
        <Item Icon={RefreshCw} label="Refresh" disabled={busy} onClick={refresh} />
        <Item
          Icon={MessageCircleQuestionMark}
          label="Ask a question"
          hint="Adds the pull request to this thread's composer."
          onClick={() => compose(`${url} `, false)}
        />
        <Item
          Icon={BookOpen}
          label="Explain this PR"
          hint="A walk through the diff and what to read closely."
          onClick={() =>
            compose(
              `Explain this pull request: ${url}. Walk me through the diff and point out what to read closely.`,
              true,
            )
          }
        />
        <Item
          Icon={Hammer}
          label="Fix findings in this thread"
          onClick={() =>
            compose(
              `Fix the review findings on this pull request: ${url}. Read its comments and checks with gh, fix what they found, and push.`,
              true,
            )
          }
        />
        {separator}
        {pr.draft ? (
          <Item
            Icon={GitPullRequest}
            label="Ready for review"
            disabled={busy || !open}
            onClick={() => act("ready")}
          />
        ) : (
          <Item
            Icon={GitPullRequestDraft}
            label="Convert to draft"
            disabled={busy || !open}
            onClick={() => act("draft")}
          />
        )}
        {pr.autoMerge ? (
          <Item
            Icon={GitMerge}
            label="Disable auto-merge"
            disabled={busy || !open}
            onClick={() => act("disableAutoMerge")}
          />
        ) : (
          <Item
            Icon={GitMerge}
            label="Enable auto-merge"
            disabled={busy || !open}
            onClick={() => act("autoMerge")}
          />
        )}
        {separator}
        <Item Icon={GitMerge} label="Merge" disabled={busy || !open} onClick={() => act("merge")} />
        <Item
          Icon={GitMerge}
          label="Squash and merge"
          disabled={busy || !open}
          onClick={() => act("squash")}
        />
        {separator}
        <Item
          Icon={ArrowUpRight}
          label="Open on GitHub"
          onClick={() => {
            menu.current?.hidePopover();
            window.open(pr.url, "_blank");
          }}
        />
        <Item Icon={Link} label="Copy link" onClick={() => copy(pr.url)} />
        <Item Icon={Copy} label="Copy PR number" onClick={() => copy(String(pr.number))} />
        {separator}
        <Item
          Icon={GitPullRequestClosed}
          label="Close pull request"
          danger
          disabled={busy || !open}
          onClick={() => {
            menu.current?.hidePopover();
            closeDialog.current?.showModal();
          }}
        />
      </div>

      <dialog
        ref={closeDialog}
        aria-labelledby={`${id}-close`}
        className="m-auto w-[24rem] rounded-xl border border-border bg-surface text-foreground shadow-composer backdrop:bg-black/50"
      >
        <form method="dialog" className="px-5 pt-4 pb-4">
          <h2 id={`${id}-close`} className="text-[15px] font-semibold">
            Close pull request #{pr.number}?
          </h2>
          <p className="mt-1.5 text-[13px] text-muted-foreground">
            “{pr.title}” closes without merging. It can be reopened on GitHub.
          </p>
          <div className="mt-4 flex justify-end gap-2">
            <button
              type="submit"
              value="cancel"
              className="rounded-md px-3 py-1.5 text-[13px] hover:bg-hover"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => {
                closeDialog.current?.close();
                act("close");
              }}
              className="rounded-md bg-red-600 px-3 py-1.5 text-[13px] font-medium text-white hover:opacity-90"
            >
              Close pull request
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}
