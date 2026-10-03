import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Ban,
  Bot,
  Brain,
  Check,
  ChevronRight,
  Copy,
  Ellipsis,
  FilePlus,
  FileText,
  Folder,
  FolderGit2,
  GitBranch,
  GitPullRequest,
  GitPullRequestArrow,
  Globe,
  Hammer,
  ImageOff,
  Info,
  ListChecks,
  Pencil,
  Plug,
  Search,
  Sparkles,
  SquareTerminal,
  TriangleAlert,
  Workflow,
  X,
  type LucideIcon,
} from "lucide-react";
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
} from "react";
import Markdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

import type { RpcError } from "../preload/bridge";
import type {
  AgentRun,
  AgentToolStatus,
  ImageId,
  JsonValue,
  PromptImage,
} from "../protocol/generated/protocol";
import {
  ApprovalDetails,
  ApprovalQueue,
  ApprovalSummary,
  DiffRow,
  diffBand,
  queueOf,
  RequestPreview,
  useAnswers,
  withAnswers,
  type Asked,
  type ToolLook,
} from "./Approval";
import { Composer, tabItem, type Unanswered } from "./Composer";
import { useConnection } from "./ConnectionStatus";
import { describeError, githubProblem } from "./errors";
import { imageCaps, imageUrl, loadImage } from "./images";
import { Loader, type LoaderStyle } from "./Loader";
import { GitHubLogo, LinearLogo } from "./logos";
import { backendOf, backends, models, type Provider, type RunOptions } from "./models";
import {
  latestPlan,
  PlanStrip,
  PlanLine,
  ProposedPlan,
  withPlans,
  type PlanRow,
  type ProposedPlanRow,
} from "./Plan";
import { plainText, PromptRail, ScrollToEnd, type Prompt } from "./PromptRail";
import { attachThreads, SentThread, ThreadLinksContext, type ThreadLinks } from "./threadContext";
import { titleOf } from "./threads";
import {
  failureText,
  groupWork,
  isRunning,
  waitingApprovals,
  workedFor,
  plxdTools,
  type Approval,
  type Item,
  type Work,
} from "./transcript";
import { SetUpGithub } from "./ui";
import { useAgentRun, type SentMessage } from "./useAgentRun";

/** A row: a transcript item, or a message this window sent that hasn't reached the agent yet. */
type Row =
  | Item
  | {
      kind: "pending";
      key: string;
      text: string;
      images?: PromptImage[];
      threads?: string[];
      turnId?: string;
    };
/** What the list shows: a turn's activity is folded into one `Work` row, its plan apart. */
type ViewRow = Row | Work | PlanRow | ProposedPlanRow;

/** Focuses the composer's editor (Composer.tsx), as when the plan strip goes with focus in it. */
const focusComposer = () => document.getElementById("composer-input")?.focus();
/** A pinned plan's Markdown, rendered as the transcript renders the agent's. */
const markdown = (text: string) => <MarkdownText text={text} />;

/**
 * The permission requests waiting on the user, pinned over the composer (RYA-196): tools named as
 * the transcript names them, and focus back to the composer once the last one goes.
 */
export function PinnedApprovals(
  props: Omit<ComponentProps<typeof ApprovalQueue>, "describe" | "markdown" | "returnFocus">,
) {
  return (
    <ApprovalQueue
      {...props}
      describe={describeTool}
      markdown={markdown}
      returnFocus={focusComposer}
    />
  );
}

/**
 * An agent run as a chat: its transcript, the composer, and the run's footer.
 * The same view serves normal threads, subagents, and a Project's coordinator.
 */
export function AgentChat({
  hostId,
  runId,
  notice,
  prompt,
  going,
  noRepo,
  tab,
  startOver,
  others,
  pullRequests,
  onPrOpened,
  onSetUpGithub,
  compose,
  onComposed,
  threadLinks,
}: {
  hostId: string;
  runId: string;
  /** A quiet note shown over the composer, such as which account a new thread got. */
  notice?: string;
  /** The run's first prompt, shown until the transcript loads, so a new thread opens on it. */
  prompt?: string;
  /**
   * Whether the run is known to be starting or running, as one this window just started is. Until
   * the transcript loads, `prompt` then shows as on its way to it, with the loader under it.
   */
  going?: boolean;
  /** A thread with no repo: its scratch repository has no origin, so it gets no Open PR. */
  noRepo?: boolean;
  /** The composer's tab in place of the run's worktree, such as a coordinator's repository. */
  tab?: ReactNode;
  /**
   * Starts a new run with `text` and `images` in place of this one once this one can't take
   * messages, as a Project's coordinator can (0024). Resolves to an error message, or undefined.
   */
  startOver?: (
    text: string,
    options: RunOptions,
    images: PromptImage[],
  ) => Promise<string | undefined>;
  /**
   * Permission requests other runs wait on, pinned here with this run's own, as a Project's other
   * runs' are in each of its chats (RYA-196).
   */
  others?: readonly Asked[];
  /** The run tab's link to its linked pull requests (PLX-319), in place of Open PR. */
  pullRequests?: ReactNode;
  /** Opens the pull request Open PR opened, in place of linking to it. */
  onPrOpened?: (url: string) => void;
  /** Offered when Open PR fails because `gh` is missing or signed out (PLX-423). */
  onSetUpGithub?: () => void;
  /**
   * A message from outside the chat, such as the PR view's: sent, or put in the composer to finish.
   * `onComposed` says it's taken, so each one is taken once.
   */
  compose?: { text: string; send: boolean };
  onComposed?: () => void;
  /**
   * The host's threads: the composer attaches them where plxd takes them (PLX-378), and the
   * transcript's attached threads open from their chips.
   */
  threadLinks?: ThreadLinks;
}) {
  const connection = useConnection(hostId);
  const connected = connection?.status === "connected";
  const { transcript, error, sent, send, cancel } = useAgentRun(hostId, runId, connected);
  // Permission requests (RYA-196): those answered here read as answered at once.
  const { answers, answer, dismiss } = useAnswers(hostId);
  const [resendError, setResendError] = useState<string>();
  const [prError, setPrError] = useState<RpcError>();
  // Open PR's failure for `gh` missing or signed out, as a short line by Set up GitHub.
  const prGithub = onSetUpGithub && githubProblem(prError);
  // Dropped follow-ups already sent again, so their Send again goes away (back on failure).
  const [resent, setResent] = useState<ReadonlySet<string>>(new Set());
  const resend = useCallback(
    (turnId: string, message: SentMessage) => {
      setResent((prev) => new Set(prev).add(turnId));
      void send(message.text, undefined, message.images, message.threads).then((failed) => {
        setResendError(failed?.message);
        if (failed)
          setResent((prev) => {
            const next = new Set(prev);
            next.delete(turnId);
            return next;
          });
      });
    },
    [send],
  );
  useEffect(() => {
    if (!compose) return;
    if (compose.send) void send(compose.text).then((failed) => setResendError(failed?.message));
    onComposed?.();
  }, [compose, send, onComposed]);
  const unsent = useMemo(
    () => new Map([...sent].filter(([turnId]) => !resent.has(turnId))),
    [sent, resent],
  );
  const { run } = transcript;
  const items = useMemo(() => withAnswers(transcript.items, answers), [transcript.items, answers]);
  const asked = useMemo(
    () =>
      queueOf(
        [...waitingApprovals(items).map((approval) => ({ runId, approval })), ...(others ?? [])],
        answers,
      ),
    [items, others, answers, runId],
  );
  const plan = useMemo(() => latestPlan(items), [items]);
  const showImage = useCallback((id: ImageId) => loadImage(hostId, runId, id), [hostId, runId]);

  // A message plxd wouldn't send because the run can't be resumed, which `startOver` can take.
  const [refused, setRefused] = useState<{
    text: string;
    options: RunOptions;
    images: PromptImage[];
    why: string;
  }>();
  const [startingOver, setStartingOver] = useState(false);
  const sendText = async (
    text: string,
    options: RunOptions,
    images: PromptImage[],
    threads: string[],
  ) => {
    const failed = await send(text, options, images, threads);
    if (!startOver || failed?.data?.kind !== "runNotResumable") return failed?.message;
    setRefused({ text, options, images, why: failed.message });
    return ""; // Back in the box; the line above it says why and offers Start over.
  };
  // A run that ended before its CLI reported a session never answered: it starts over with its
  // own first message. ponytail: without that message's images, which would need fetching first.
  const stuck =
    startOver &&
    (refused ??
      (run && !isRunning(run.status) && !run.sessionId
        ? {
            text: run.prompt,
            options: {},
            images: [],
            why: "it stopped before its session started.",
          }
        : undefined));
  const restart = async () => {
    if (!stuck) return;
    setStartingOver(true);
    // The new run keeps this one's model, effort, and mode unless the message changed them.
    const {
      model = run?.model,
      effort = run?.effort,
      permission = run?.permission,
    } = stuck.options;
    const failed = await startOver(stuck.text, { model, effort, permission }, stuck.images);
    setStartingOver(false);
    if (failed) setRefused({ ...stuck, why: failed });
  };

  // Sent from here, but no turnStarted (or followUpDropped) for it yet.
  const rows = useMemo<Row[]>(() => {
    const seen = new Set(items.flatMap((i) => ("turnId" in i && i.turnId ? [i.turnId] : [])));
    const pending = [...sent]
      .filter(([turnId]) => !seen.has(turnId))
      .map(([turnId, { text, images, threads }]) => ({
        kind: "pending" as const,
        key: `pending:${turnId}`,
        text,
        images,
        threads,
        turnId,
      }));
    const all = [...items, ...pending];
    if (all.length > 0 || !prompt) return all;
    return [
      going
        ? { kind: "pending", key: "pending:prompt", text: prompt }
        : { kind: "user", key: "prompt", text: prompt },
    ];
  }, [items, sent, prompt, going]);
  // The user's prompts, for the composer's Up: not Parallax's wake-ups.
  const history = useMemo(
    () =>
      rows.flatMap((row) => {
        if (row.kind === "pending") return [row.text];
        if (row.kind !== "user" || row.wake) return [];
        const text = row.text ?? (row.turnId && sent.get(row.turnId)?.text);
        return text ? [text] : [];
      }),
    [rows, sent],
  );
  // The latest prompt while nothing from the agent follows it, which Stop puts back in the box:
  // its text, its attached threads, and its images: at hand when sent from here, or fetched from
  // plxd by id on Stop.
  const unanswered = useMemo<(Unanswered & { turnId?: string }) | undefined>(() => {
    const at = rows.findLastIndex((r) => r.kind === "user" || r.kind === "pending");
    const row = rows[at];
    if ((row?.kind !== "user" && row?.kind !== "pending") || (row.kind === "user" && row.wake))
      return undefined;
    if (!rows.slice(at + 1).every((r) => ["notice", "end", "session"].includes(r.kind)))
      return undefined;
    const mine = row.kind === "user" && row.turnId ? sent.get(row.turnId) : undefined;
    const text = row.text ?? mine?.text;
    if (text == null) return undefined;
    const atHand = (row.kind === "pending" ? row.images : mine?.images) ?? [];
    const ids = row.kind === "user" && !mine ? (row.images ?? []) : [];
    const images = async () => {
      const got = await Promise.all(
        ids.map((imageId) =>
          window.parallax.request(hostId, "agent/image", { runId, imageId }).catch(() => undefined),
        ),
      );
      return [...atHand, ...got.flatMap((a) => (a && "result" in a ? [a.result] : []))];
    };
    const threads = row.threads ?? mine?.threads;
    // Only one still on its way can be dropped, and so offer Send again.
    return { text, images, threads, turnId: row.kind === "pending" ? row.turnId : undefined };
  }, [rows, sent, hostId, runId]);
  // A stopped prompt goes back in the box, so if plxd drops it, it offers no Send again too.
  const stop = async () => {
    const back = unanswered;
    const failed = await cancel();
    if (!failed && back?.turnId) setResent((prev) => new Set(prev).add(back.turnId!));
    return failed;
  };

  let disabledReason: string | undefined;
  if (connection?.status === "failed") disabledReason = "Disconnected from plxd";
  else if (!connected) disabledReason = "Connecting to plxd…";
  else if (!run) disabledReason = error ? "This chat couldn't load" : "Loading…";
  let optionsDisabled: string | undefined;
  // `sendModel` is `sendOptions`' successor, which also takes the model (RYA-163). With
  // `sendAccount`, a message that changes them while the run works waits for it to finish.
  const moves = connected && "sendAccount" in connection.capabilities;
  if (connected && !("sendModel" in connection.capabilities))
    optionsDisabled = "This host's plxd can't change a thread's model, effort, or access";
  else if (isRunning(run?.status) && !moves)
    optionsDisabled = "The model, effort, and access can change once it finishes";
  // The providers the run can't move to, by why: any but its own on a plxd that can't move runs,
  // and for a coordinator, those whose backend can't run one.
  const unavailable: Partial<Record<Provider, string>> = {};
  const own = run && backends[run.backend]?.provider;
  for (const p of new Set(models.map((m) => m.provider)))
    if (p === own) continue;
    else if (!moves)
      unavailable[p] =
        `${p} is unavailable in this thread. Start a new thread to switch providers.`;
    else if (run?.policy === "noWrite" && !backends[backendOf(p)]?.coordinator)
      unavailable[p] = `${p} can't run a Project's coordinator yet.`;
  // Manual's requests come here only from a run that asked for them, on a plxd that sends them.
  let manualDenied: "host" | "run" | undefined;
  if (connected && !("approvals" in connection.capabilities)) manualDenied = "host";
  else if (run && !run.approvals) manualDenied = "run";
  // A finished run with a commit can go to GitHub (RYA-168), until Accept removes its branch.
  const canOpenPr =
    connected &&
    "openPr" in connection.capabilities &&
    !noRepo &&
    !!run?.diff &&
    !isRunning(run.status) &&
    run.status !== "accepted";

  // One that couldn't load, stopped updating, or lost plxd shows nothing in progress.
  const stalled = error !== undefined || (connection !== undefined && !connected);

  return (
    <>
      {rows.length > 0 ? (
        <ThreadLinksContext value={threadLinks}>
          <TranscriptView
            rows={rows}
            sent={unsent}
            live={isRunning(run?.status)}
            stalled={stalled}
            onResend={resend}
            loadImage={showImage}
          />
        </ThreadLinksContext>
      ) : (
        <div className="flex flex-1 flex-col items-center justify-center gap-1 px-8 text-center text-[13px] text-faint-foreground">
          {error ? (
            <>
              <p className="font-medium text-foreground">This chat couldn't load</p>
              <p className="text-muted-foreground">{error}</p>
            </>
          ) : (
            connected && "Loading…"
          )}
        </div>
      )}
      {/* A column the window bounds, so a pinned card's preview gives way to a grown composer. */}
      <div className="mx-auto flex min-h-0 w-full max-w-3xl flex-col px-6 pb-5">
        {/* Requests waiting on the user, pinned so they can't scroll away. */}
        <PinnedApprovals
          asked={asked}
          answers={answers}
          onAnswer={(a, choice, message) => void answer(a, choice, message)}
          onDismiss={dismiss}
          disabledReason={connected ? undefined : (disabledReason ?? "Connecting to plxd…")}
        />
        {/* A loaded transcript that stopped updating, a failed Send again, or Open PR. */}
        {(error ?? resendError ?? prError) && rows.length > 0 && (
          <p role="alert" className="flex items-center gap-2 px-2 pb-2 text-[12.5px] text-danger">
            {error ?? resendError ?? (prGithub || describeError(prError!))}
            {!error && !resendError && prGithub && <SetUpGithub onClick={onSetUpGithub!} />}
          </p>
        )}
        {notice && (
          <p role="status" className="px-2 pb-2 text-[12.5px] text-muted-foreground">
            {notice}
          </p>
        )}
        {stuck && (
          <p role="alert" className="px-2 pb-2 text-[12.5px] text-danger">
            This chat can't continue: {stuck.why}{" "}
            <button
              type="button"
              disabled={startingOver}
              onClick={() => void restart()}
              className="font-medium text-foreground underline underline-offset-2 disabled:opacity-50"
            >
              Start over
            </button>
          </p>
        )}
        {/* The latest turn's plan, while the run works on it. */}
        {plan && isRunning(run?.status) && !stalled && (
          <PlanStrip
            items={plan.items}
            active={plan.active}
            loader={loaders.planning}
            returnFocus={focusComposer}
          />
        )}
        <Composer
          onSend={sendText}
          history={history}
          onStop={isRunning(run?.status) ? stop : undefined}
          unanswered={unanswered}
          disabledReason={disabledReason}
          tab={
            tab ??
            (run && (
              <RunTab run={run}>
                {pullRequests ||
                  (canOpenPr && (
                    <OpenPr hostId={hostId} run={run} onError={setPrError} onOpened={onPrOpened} />
                  ))}
              </RunTab>
            ))
          }
          backend={run?.backend}
          started={run}
          contextAndFast={connected && "contextAndFast" in connection.capabilities}
          unavailable={unavailable}
          optionsDisabled={optionsDisabled}
          imageCaps={imageCaps(connection)}
          manualDenied={manualDenied}
          insert={compose && !compose.send ? compose.text : undefined}
          menus={
            connected && "composerMenus" in connection.capabilities ? { hostId, runId } : undefined
          }
          attach={attachThreads(connection, threadLinks, runId)}
        />
      </div>
    </>
  );
}

/**
 * The transcript as a virtualized list. It follows new output while scrolled to
 * the bottom, and stays put once the user scrolls up.
 */
export function TranscriptView({
  rows,
  sent,
  live,
  stalled = false,
  onResend,
  loadImage,
}: {
  rows: Row[];
  sent: ReadonlyMap<string, SentMessage>;
  live: boolean;
  /** Whether the transcript is out of date, so nothing in it shows as in progress. */
  stalled?: boolean;
  onResend?: (turnId: string, message: SentMessage) => void;
  /** Fetches a message's image by id, as a data URL. */
  loadImage?: (imageId: ImageId) => Promise<string | undefined>;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  // Whether Scroll to end's smooth scroll is on its way down.
  const ending = useRef(false);
  // Which tool calls and thoughts are expanded, kept here since rows unmount off screen.
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const toggle = useCallback(
    (key: string, next: boolean) =>
      setOpen((prev) => {
        const set = new Set(prev);
        if (next) set.add(key);
        else set.delete(key);
        return set;
      }),
    [],
  );

  const going = live && !stalled;
  // A work row with nothing in it follows a message on its way to the agent, or one it has but
  // hasn't answered while the run goes, standing for what the agent is doing before it does
  // anything. It goes before any notices after the message, where the work it stands for will be.
  const view = useMemo(() => {
    const grouped = groupWork(withPlanApprovals(withPlans(rows)));
    const at = grouped.findLastIndex((r) => r.kind !== "notice");
    const last = grouped[at];
    // A plan stands apart from the work around it, so work goes on after it as after a message,
    // and so does an answered request. One still waiting holds the agent until it's answered.
    const waits =
      ["user", "plan", "proposedPlan"].includes(last?.kind ?? "") ||
      (last?.kind === "approval" && !!last.resolved);
    if (!stalled && (last?.kind === "pending" || (live && waits)))
      grouped.splice(at + 1, 0, { kind: "work", key: "work:pending", items: [] });
    return grouped;
  }, [rows, live, stalled]);
  // The agent's text streams in its own row, so a work row is only live while it is the last
  // (a notice after it doesn't count). The empty one is, even before the run goes again.
  const tail = view.findLastIndex((r) => r.kind !== "notice");
  const activeIndex =
    view[tail]?.kind === "work" && (going || view[tail].key === "work:pending") ? tail : -1;

  const virtualizer = useVirtualizer({
    count: view.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 64,
    overscan: 8,
    paddingStart: 16,
    paddingEnd: 24,
    getItemKey: (i) => view[i]!.key,
  });
  const total = virtualizer.getTotalSize();
  // Whether it's scrolled up from the end, which offers Scroll to end.
  const [scrolledUp, setScrolledUp] = useState(false);
  // The user's prompts, for the rail, and which one is being read.
  const prompts = useMemo(() => promptsOf(view, sent), [view, sent]);
  const [reading, setReading] = useState(-1);
  // The prompt being read: the last one starting above the viewport's top third, so a prompt
  // counts once its reply fills the view, and at the end, the latest.
  const follow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const line = el.scrollTop + el.clientHeight / 3;
    const starts = virtualizer.measurementsCache;
    const at = atBottom.current
      ? prompts.length - 1
      : prompts.findLastIndex((p) => (starts[p.index]?.start ?? Infinity) <= line);
    setReading(Math.max(0, at));
  }, [prompts, virtualizer]);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
    follow();
  }, [total, view.length, follow]);
  // As the composer grows it shrinks the list from below: keep the latest output in view.
  useEffect(() => {
    const el = scrollRef.current!;
    const observer = new ResizeObserver(() => {
      if (atBottom.current) el.scrollTop = el.scrollHeight;
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const scrollToEnd = () => {
    const el = scrollRef.current;
    if (!el) return;
    ending.current = true;
    setScrolledUp(false);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };
  const jump = (prompt: Prompt) => {
    ending.current = false;
    virtualizer.scrollToIndex(prompt.index, { align: "start" });
  };

  return (
    // Bounds the rail and Scroll to end, which stay put while the list scrolls under them.
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        role="log"
        aria-label="Transcript"
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
          if (atBottom.current) ending.current = false;
          // Scroll to end's smooth scroll passes through the middle, where it stays hidden.
          if (!ending.current) setScrolledUp(!atBottom.current);
          follow();
        }}
        // Scrolling by hand cuts Scroll to end's scroll short.
        onWheel={() => (ending.current = false)}
        onPointerDown={() => (ending.current = false)}
        onKeyDown={() => (ending.current = false)}
        className="min-h-0 flex-1 overflow-y-auto select-text"
      >
        <div className="relative w-full" style={{ height: total }}>
          {virtualizer.getVirtualItems().map((v) => {
            const row = view[v.index]!;
            return (
              <div
                key={v.key}
                data-index={v.index}
                ref={virtualizer.measureElement}
                className="absolute top-0 left-0 w-full"
                style={{ transform: `translateY(${v.start}px)` }}
              >
                <div className="mx-auto max-w-3xl px-6 py-2">
                  <RowView
                    row={row}
                    sent={"turnId" in row && row.turnId ? sent.get(row.turnId) : undefined}
                    live={going}
                    open={open.has(row.key)}
                    openKeys={row.kind === "work" ? open : undefined}
                    active={v.index === activeIndex}
                    onToggle={toggle}
                    onResend={onResend}
                    loadImage={loadImage}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </div>
      {/* One prompt is no choice of where to go, so there's no rail for it. */}
      {prompts.length > 1 && <PromptRail prompts={prompts} current={reading} onJump={jump} />}
      {scrolledUp && <ScrollToEnd onClick={scrollToEnd} />}
    </div>
  );
}

/**
 * The user's prompts among `view`'s rows, for the rail: a follow-up's text from `sent` when the
 * log lacks it, and the start of the agent's last reply before the next prompt. Parallax's own
 * wake-ups aren't the user's, and replies to them aren't replies to the prompt before.
 */
function promptsOf(view: readonly ViewRow[], sent: ReadonlyMap<string, SentMessage>): Prompt[] {
  const prompts: Prompt[] = [];
  let last: Prompt | undefined;
  view.forEach((row, index) => {
    if (row.kind === "user" && row.wake) last = undefined;
    else if (row.kind === "user" || row.kind === "pending") {
      const said = row.text ?? (row.kind === "user" && row.turnId && sent.get(row.turnId)?.text);
      const text = said ? plainText(said) : "";
      last = { index, text: text || (row.images?.length ? "Image" : "Follow-up message") };
      prompts.push(last);
    } else if (row.kind === "assistant" && last) {
      const reply = plainText(row.text.split(/\n\s*\n/).find((p) => p.trim()) ?? "");
      if (reply) last.reply = reply;
    }
  });
  return prompts;
}

// A proposed plan the user sent back, by its row, so the row keeps its object (RowView's memo).
const sentBack = new WeakMap<ProposedPlanRow, ProposedPlanRow>();

/**
 * Proposed plans with their `ExitPlanMode` requests (RYA-196): one still waiting is pinned over the
 * composer instead, so it doesn't show twice, and one the user sent back reads as not approved,
 * however its call ended.
 */
function withPlanApprovals(rows: Exclude<ViewRow, Work>[]): Exclude<ViewRow, Work>[] {
  const asked = new Map<string, Approval>();
  for (const row of rows)
    if (row.kind === "approval" && row.request.toolName === "ExitPlanMode" && row.request.callId)
      asked.set(row.request.callId, row);
  if (asked.size === 0) return rows;
  return rows.flatMap((row): Exclude<ViewRow, Work>[] => {
    if (row.kind !== "proposedPlan") return [row];
    const approval = asked.get(row.callId);
    if (approval && !approval.resolved) return [];
    const { decision, by } = approval?.resolved ?? {};
    if (decision !== "denied" || by !== "user" || row.status === "denied") return [row];
    const denied = sentBack.get(row) ?? { ...row, status: "denied" as const };
    sentBack.set(row, denied);
    return [denied];
  });
}

interface RowProps {
  row: ViewRow;
  /** A follow-up this window sent: its text, which an older log lacks, and its images, at hand. */
  sent?: SentMessage;
  /** Whether the run is going, so a tool call with no result is still in progress. */
  live: boolean;
  open: boolean;
  /** For a work row: which of its items are expanded. */
  openKeys?: ReadonlySet<string>;
  /** For a work row: whether it is the one the agent is working in now. */
  active?: boolean;
  onToggle: (key: string, open: boolean) => void;
  /** Sends a dropped follow-up again. */
  onResend?: (turnId: string, message: SentMessage) => void;
  /** Fetches a message's image by id, as a data URL. */
  loadImage?: (imageId: ImageId) => Promise<string | undefined>;
}

/** One transcript row. Memoized: an unchanged item keeps its object, so it skips re-rendering. */
export const RowView = memo(function RowView({
  row,
  sent,
  live,
  open,
  openKeys,
  active,
  onToggle,
  onResend,
  loadImage,
}: RowProps) {
  switch (row.kind) {
    case "work":
      return (
        <WorkGroup
          work={row}
          active={active ?? false}
          live={live}
          open={open}
          openKeys={openKeys ?? new Set()}
          onToggle={onToggle}
        />
      );
    case "user":
    case "pending": {
      // A wake-up is Parallax's message to the coordinator, not the user's (0025).
      if (row.kind === "user" && row.wake)
        return (
          <Disclosure
            id={row.key}
            open={open}
            onToggle={onToggle}
            summary={
              <span className="flex items-center gap-1.5 text-muted-foreground">
                <Workflow aria-hidden className="size-3.5" />
                From Parallax: subagents finished
              </span>
            }
          >
            <p className="text-[13px] leading-relaxed whitespace-pre-wrap text-muted-foreground">
              {row.text}
            </p>
          </Disclosure>
        );
      const text = row.text ?? sent?.text;
      // Images sent from here are at hand; the log's come from plxd by id.
      const images = row.kind === "pending" ? row.images : (sent?.images ?? row.images);
      const threads = row.threads ?? sent?.threads;
      return (
        <div className="group/prompt flex flex-col items-end gap-1.5">
          {images && images.length > 0 && (
            <div className="flex max-w-[85%] flex-wrap justify-end gap-1.5">
              {images.map((image, i) => (
                <MessageImage key={i} image={image} loadImage={loadImage} />
              ))}
            </div>
          )}
          {/* The threads attached as context, which open from here (PLX-378). */}
          {threads && threads.length > 0 && (
            <div className="flex max-w-[85%] flex-wrap justify-end gap-1.5">
              {threads.map((id) => (
                <SentThread key={id} runId={id} />
              ))}
            </div>
          )}
          {/* A message of images alone has no bubble. */}
          {(!images?.length || text?.trim() !== "") && (
            <div className="max-w-[85%] rounded-2xl bg-selected px-3.5 py-2 text-[14px] leading-relaxed whitespace-pre-wrap">
              {/* A follow-up from an older log, which has no text for it. */}
              {text ?? <span className="text-muted-foreground italic">Follow-up message</span>}
            </div>
          )}
          <PromptMeta at={row.kind === "user" ? row.at : undefined} text={text} />
        </div>
      );
    }
    case "assistant":
      return <MarkdownText text={row.text} />;
    case "reasoning":
      return (
        <Disclosure
          id={row.key}
          open={open}
          onToggle={onToggle}
          summary={
            <>
              <icons.thinking aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="text-muted-foreground">Thinking</span>
            </>
          }
        >
          <p className="text-[13px] leading-relaxed whitespace-pre-wrap text-muted-foreground">
            {row.text}
          </p>
        </Disclosure>
      );
    case "tool":
      return <ToolCall item={row} live={live} open={open} onToggle={onToggle} />;
    case "plan":
    case "todo":
      // The turn's plan and its later updates, as lines: the strip shows the whole list.
      return <PlanLine item={row} />;
    case "proposedPlan":
      return (
        <ProposedPlan id={row.key} status={row.status} open={open} onToggle={onToggle}>
          <MarkdownText text={row.plan} />
        </ProposedPlan>
      );
    case "approval":
      // A permission request's line: waiting, then how it was answered (RYA-196).
      return (
        <Disclosure
          id={row.key}
          open={open}
          onToggle={onToggle}
          summary={
            <ApprovalSummary
              approval={row}
              tool={describeTool(row.request.toolName, row.request.input)}
            />
          }
        >
          <ApprovalDetails approval={row} />
        </Disclosure>
      );
    case "notice":
      return (
        <p className="flex items-start gap-2 text-[12.5px] text-muted-foreground">
          {row.tone === "warning" ? (
            <TriangleAlert aria-hidden className="mt-0.5 size-3.5 shrink-0" />
          ) : (
            <Info aria-hidden className="mt-0.5 size-3.5 shrink-0" />
          )}
          <span>
            {row.text}
            {/* A dropped follow-up this window sent: offer it again, rather than lose it. */}
            {row.turnId && sent !== undefined && onResend && (
              <>
                {" "}
                <button
                  type="button"
                  onClick={() => onResend(row.turnId!, sent)}
                  className="font-medium text-foreground underline underline-offset-2"
                >
                  Send again
                </button>
              </>
            )}
          </span>
        </p>
      );
    case "end": {
      const { outcome } = row;
      if (outcome.status === "failed") {
        // A coordinator's no-write stop lists what it changed after its first line, one
        // `git status` line each (0024).
        const [first, ...changed] = outcome.message.split("\n");
        return (
          <div
            role="alert"
            className="rounded-lg border border-danger/30 px-3.5 py-2.5 text-[13px]"
          >
            <p className="font-medium text-danger">Failed: {failureText(outcome.failure)}</p>
            <p className="mt-0.5 text-muted-foreground">{first}</p>
            {changed.length > 0 && (
              <pre className="mt-2 max-h-60 overflow-auto rounded-xl border border-border bg-code px-3 py-2 font-mono text-[12px]">
                {changed.join("\n")}
              </pre>
            )}
          </div>
        );
      }
      const label =
        outcome.status === "completed"
          ? "Done"
          : outcome.status === "cancelled"
            ? "Stopped"
            : outcome.status === "interrupted"
              ? "Interrupted when plxd stopped. Send a message to pick up where it left off."
              : "Ended";
      return (
        <div className="flex items-center gap-3 text-[12px] text-faint-foreground">
          <span className="h-px flex-1 bg-border" />
          {label}
          <span className="h-px flex-1 bg-border" />
        </div>
      );
    }
  }
});

/**
 * One of a user message's images, as a thumbnail: at hand, or fetched by id. The same height
 * either way, so the row doesn't jump when it loads.
 */
function MessageImage({
  image,
  loadImage,
}: {
  image: PromptImage | ImageId;
  loadImage?: (imageId: ImageId) => Promise<string | undefined>;
}) {
  // By id: its data URL once fetched, or null when plxd couldn't serve it.
  const [fetched, setFetched] = useState<{ id: ImageId; url: string | null }>();
  useEffect(() => {
    if (typeof image !== "string" || !loadImage) return;
    let live = true;
    void loadImage(image).then((url) => live && setFetched({ id: image, url: url ?? null }));
    return () => {
      live = false;
    };
  }, [image, loadImage]);
  const url =
    typeof image === "string" ? (fetched?.id === image ? fetched.url : undefined) : imageUrl(image);
  if (url)
    return (
      <img
        src={url}
        alt="Image"
        className="h-32 max-w-60 rounded-xl border border-border object-cover"
      />
    );
  return (
    <span
      role="img"
      aria-label={url === null ? "Image unavailable" : "Loading image"}
      className="grid h-32 w-32 place-items-center rounded-xl border border-border bg-selected text-faint-foreground"
    >
      {url === null && <ImageOff aria-hidden className="size-5" />}
    </span>
  );
}

/**
 * A run of thinking, tool calls, and checklists under one dropdown. While the agent works its
 * header says what it's doing now, with a loader for that; afterward it says how long it worked,
 * and hides the rest. Before the agent does anything, it's empty and muses.
 */
function WorkGroup({
  work,
  active,
  live,
  open,
  openKeys,
  onToggle,
}: {
  work: Work;
  active: boolean;
  live: boolean;
  open: boolean;
  openKeys: ReadonlySet<string>;
  onToggle: (key: string, open: boolean) => void;
}) {
  const now = active ? activity(work.items.at(-1)) : undefined;
  return (
    <div>
      <button
        type="button"
        aria-expanded={open}
        disabled={work.items.length === 0}
        onClick={() => onToggle(work.key, !open)}
        className="group/work flex max-w-full cursor-default items-center gap-1.5 rounded-md py-0.5 text-[13px] hover:text-foreground"
      >
        {now && work.items.length === 0 ? (
          <Musing />
        ) : now ? (
          <>
            <Loader {...now.loader} />
            {/* Keyed, so a new label fades in. */}
            <span key={now.label} className="working-in shrink-0">
              <Shimmer>{now.label}</Shimmer>
            </span>
            {now.detail && <span className="truncate text-muted-foreground">{now.detail}</span>}
          </>
        ) : (
          <span className="text-muted-foreground">{workedFor(work.startedAt, work.endedAt)}</span>
        )}
        {work.items.length > 0 && (
          <ChevronRight
            aria-hidden
            className={`size-3.5 shrink-0 text-faint-foreground transition-transform ${open ? "rotate-90" : ""}`}
          />
        )}
      </button>
      {open && (
        <div className="mt-2 ml-1 space-y-2.5 border-l border-border pl-4">
          {work.items.map((item) => (
            <RowView
              key={item.key}
              row={item}
              live={live}
              open={openKeys.has(item.key)}
              onToggle={onToggle}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// A word for what the agent might be doing before it does anything, and how often it changes.
const musings = [
  "Picturing",
  "Pondering",
  "Sketching",
  "Mulling",
  "Brewing",
  "Tinkering",
  "Percolating",
  "Noodling",
];
const musingMs = 2400;

/**
 * The working line before the agent does anything: a loader and a word that changes every few
 * seconds, which screen readers hear as a steady "Working".
 */
function Musing() {
  // Picked by the wall clock, so a remount (New Thread handing off to the opened thread) keeps the
  // sequence. A word fades in, and the last one out, only once it changes here. Under reduced
  // motion it keeps one.
  const [tick, setTick] = useState(() => Math.floor(Date.now() / musingMs));
  const [first] = useState(tick);
  useEffect(() => {
    if (document.documentElement.classList.contains("reduce-motion")) return;
    const timer = setTimeout(
      () => setTick(Math.max(tick + 1, Math.floor(Date.now() / musingMs))),
      musingMs - (Date.now() % musingMs),
    );
    return () => clearTimeout(timer);
  }, [tick]);
  const word = (at: number) => musings[at % musings.length]!;
  return (
    <>
      <Loader {...loaders.working} />
      <span className="sr-only">Working</span>
      <span aria-hidden className="grid shrink-0 justify-items-start">
        {tick > first && (
          <span key={tick - 1} className="working-out [grid-area:1/1]">
            <Shimmer>{word(tick - 1)}</Shimmer>
          </span>
        )}
        <span key={tick} className={`[grid-area:1/1] ${tick > first ? "working-in" : ""}`}>
          <Shimmer>{word(tick)}</Shimmer>
        </span>
      </span>
    </>
  );
}

// How long the shimmer takes to sweep across its text.
const shimmerMs = 2000;

/** Text with a soft highlight sweeping across it, on the wall clock so a remount carries on. */
function Shimmer({ children }: { children: string }) {
  const [now] = useState(Date.now);
  return (
    <span
      className="working-shimmer"
      style={{ animationDuration: `${shimmerMs}ms`, animationDelay: `${-(now % shimmerMs)}ms` }}
    >
      {children}
    </span>
  );
}

// What a tool is doing, in a word, by the names common tools use.
const verbs: Partial<Record<string, string>> = {
  Bash: "Running",
  Read: "Reading",
  Grep: "Searching",
  Glob: "Searching",
  WebSearch: "Searching",
  WebFetch: "Fetching",
  Edit: "Editing",
  MultiEdit: "Editing",
  Write: "Writing",
  Task: "Running agent",
  Agent: "Running agent",
};

/** The loader for each kind of work. */
const loaders = {
  thinking: { kind: "matrix", variant: "ripple" },
  shell: { kind: "register", variant: "shift" },
  reading: { kind: "bands", variant: "descend" },
  searching: { kind: "matrix", variant: "scan" },
  editing: { kind: "cells", variant: "merge" },
  writing: { kind: "cells", variant: "merge" },
  fetching: { kind: "beacon", variant: "rise" },
  agent: { kind: "orbit", variant: "oppose" },
  skill: { kind: "lift", variant: "rise" },
  mcp: { kind: "beacon", variant: "balance" },
  plxd: { kind: "cells", variant: "spread" },
  planning: { kind: "lift", variant: "breathe" },
  working: { kind: "orbit", variant: "chase" },
} as const satisfies Record<string, LoaderStyle>;

type Kind = keyof typeof loaders;

/** The icon for each kind of work, in place of its loader once it's done. */
const icons = {
  thinking: Brain,
  shell: SquareTerminal,
  reading: FileText,
  searching: Search,
  editing: Pencil,
  writing: FilePlus,
  fetching: Globe,
  agent: Bot,
  skill: Sparkles,
  mcp: Plug,
  plxd: Workflow,
  planning: ListChecks,
  working: Hammer,
} as const satisfies Record<Kind, LucideIcon>;

// The kind of work a tool does, by the names common tools use.
const toolKinds: Partial<Record<string, Kind>> = {
  Bash: "shell",
  Read: "reading",
  NotebookRead: "reading",
  LS: "reading",
  Grep: "searching",
  Glob: "searching",
  WebSearch: "searching",
  ToolSearch: "searching",
  Edit: "editing",
  MultiEdit: "editing",
  NotebookEdit: "editing",
  Write: "writing",
  WebFetch: "fetching",
  Task: "agent",
  Agent: "agent",
  Skill: "skill",
  TodoWrite: "planning",
  // Claude Code's task tools, which keep its plan in place of TodoWrite (RYA-248).
  TaskCreate: "planning",
  TaskUpdate: "planning",
  TaskList: "planning",
  TaskGet: "planning",
};

/** The kind of work a tool call does: a plxd or other MCP server's tool, or by its name. */
function toolKind(item: Extract<Item, { kind: "tool" }>): Kind {
  if (item.name?.startsWith(plxdTools)) return "plxd";
  if (mcpTool(item.name)) return "mcp";
  // The table's own names only, so "constructor" or "toString" is any other tool.
  const name = item.name ?? "";
  return (Object.hasOwn(toolKinds, name) && toolKinds[name]) || "working";
}

/** What the agent is doing: a label, what it's doing it to, and the loader drawn beside them. */
export interface Activity {
  label: string;
  detail?: string;
  loader: LoaderStyle;
}

/** The header of the work in progress: what its latest item is doing. */
export function activity(item?: Item): Activity {
  switch (item?.kind) {
    case "reasoning":
      return { label: "Thinking", loader: loaders.thinking };
    case "tool": {
      const loader = loaders[toolKind(item)];
      const plxd = plxdCall(item);
      if (plxd) return { ...plxd, loader };
      // Including a plxd tool this app doesn't know.
      const mcp = mcpTool(item.name);
      if (mcp) return { label: `Using ${mcp.server}`, detail: mcp.tool, loader };
      if (item.name === "Skill")
        return { label: "Using skill", detail: skillName(item.input), loader };
      return {
        label: verbs[item.name ?? ""] ?? item.name ?? "Working",
        detail: toolHint(item.input),
        loader,
      };
    }
    case "todo":
      return { label: "Planning", loader: loaders.planning };
    default:
      return { label: "Working", loader: loaders.working };
  }
}

function ToolCall({
  item,
  live,
  open,
  onToggle,
}: {
  item: Extract<Item, { kind: "tool" }>;
  live: boolean;
  open: boolean;
  onToggle: (key: string, open: boolean) => void;
}) {
  const named = plxdCall(item) ?? namedTool(item);
  const kind = toolKind(item);
  const status = item.status ?? (live ? "running" : "none");
  // A status newer than this app reads as no result.
  const look = (statuses as Partial<Record<string, Look>>)[status] ?? statuses.none;
  const Icon = icons[kind];
  return (
    <Disclosure
      id={item.key}
      open={open}
      onToggle={onToggle}
      summary={
        <>
          {/* Its kind: the loader while it runs, then the icon, with a mark unless it succeeded. */}
          <span aria-hidden className={`relative shrink-0 ${look.color}`}>
            {status === "running" ? (
              <Loader {...loaders[kind]} size={14} />
            ) : (
              <Icon className={`size-3.5 ${look.faded ? "opacity-50" : ""}`} />
            )}
            {look.mark && (
              <look.mark
                strokeWidth={3}
                className="absolute -right-1.5 -bottom-1.5 size-2.5 rounded-full bg-background"
              />
            )}
          </span>
          <span className="shrink-0 font-medium">{named?.label ?? item.name ?? "Tool"}</span>
          {named ? (
            <span className="truncate text-muted-foreground">{named.detail}</span>
          ) : (
            <span className="truncate font-mono text-[12px] text-muted-foreground">
              {toolHint(item.input)}
            </span>
          )}
          <span className="sr-only">{look.said}</span>
        </>
      }
    >
      <div className="space-y-2 text-[12px]">
        {item.name && previewed.has(item.name) && isWhole(item.input) ? (
          <div>
            <p className="mb-1 text-[11.5px] text-faint-foreground">Input</p>
            <RequestPreview request={{ toolName: item.name, input: item.input }} />
          </div>
        ) : (
          item.input !== undefined && <Block label="Input">{inputText(item.input)}</Block>
        )}
        {item.output !== undefined && <Block label="Output">{item.output}</Block>}
      </div>
    </Disclosure>
  );
}

/** How a tool call went: its icon's color, mark, and fading, and in words for screen readers. */
interface Look {
  color: string;
  mark?: LucideIcon;
  /** Whether the icon fades, as for a call that never finished. */
  faded?: boolean;
  said: string;
}

// An unfinished call's icon is half its faint color, so it stands apart from a finished one's
// muted icon, and its mark is an ellipsis, which no round icon can swallow as a ring could.
const statuses = {
  running: { color: "", said: "Running" },
  ok: { color: "text-muted-foreground", said: "Succeeded" },
  error: { color: "text-danger", mark: X, said: "Failed" },
  denied: { color: "text-danger", mark: Ban, said: "Denied" },
  none: { color: "text-faint-foreground", mark: Ellipsis, faded: true, said: "No result" },
} satisfies Record<"running" | AgentToolStatus | "none", Look>;

/** A collapsed-by-default row, open state kept by the transcript. */
function Disclosure({
  id,
  open,
  onToggle,
  summary,
  children,
}: {
  id: string;
  open: boolean;
  onToggle: (key: string, open: boolean) => void;
  summary: ReactNode;
  children: ReactNode;
}) {
  return (
    <details
      open={open}
      onToggle={(e) => e.currentTarget.open !== open && onToggle(id, e.currentTarget.open)}
      className="group/disclosure"
    >
      <summary className="flex cursor-default list-none items-center gap-2 rounded-md py-0.5 text-[13px] hover:text-foreground [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden
          className="size-3.5 shrink-0 text-faint-foreground transition-transform group-open/disclosure:rotate-90"
        />
        {summary}
      </summary>
      <div className="mt-1.5 ml-5.5">{children}</div>
    </details>
  );
}

function Block({ label, children }: { label: string; children: string }) {
  return (
    <div>
      <p className="mb-1 text-[11.5px] text-faint-foreground">{label}</p>
      <pre className="max-h-80 overflow-auto rounded-xl border border-border bg-code px-3 py-2 font-mono leading-relaxed whitespace-pre-wrap">
        {children}
      </pre>
    </div>
  );
}

// The input field that says what a call does, by the names common tools use.
const hintFields = ["command", "file_path", "path", "pattern", "url", "query", "description"];

function toolHint(input?: JsonValue, fields = hintFields): string {
  if (!input || typeof input !== "object" || Array.isArray(input)) return "";
  const value = fields.map((f) => input[f]).find((v) => typeof v === "string");
  return typeof value === "string" ? (value.split("\n")[0] ?? "") : "";
}

// A coordinator's plxd tools (0019), by what they did.
const plxdLabels: Partial<Record<string, string>> = {
  spawn_agent: "Started a subagent",
  list_agents: "Listed subagents",
  agent_status: "Checked on a subagent",
  message_agent: "Messaged a subagent",
  cancel_agent: "Stopped a subagent",
  agent_diff: "Read a subagent's diff",
  read_context: "Read shared context",
  write_context: "Wrote shared context",
};

/**
 * A plxd tool call as a short line: what it did, and what it did it to (the new subagent's task,
 * the subagent it named, or the context file). Undefined for any other tool.
 */
function plxdCall(item: Extract<Item, { kind: "tool" }>) {
  const label = item.name?.startsWith(plxdTools)
    ? plxdLabels[item.name.slice(plxdTools.length)]
    : undefined;
  return label
    ? { label, detail: item.subagent ?? toolHint(item.input, ["prompt", "path"]) }
    : undefined;
}

/**
 * An MCP server's tool as Claude Code names it, `mcp__<server>__<tool>`, readably: the server's
 * name, capitalized unless it's plxd's, and the tool's. Undefined for any other tool.
 */
function mcpTool(name: string | null) {
  const [, server, tool] = /^mcp__(.+?)__(.+)$/.exec(name ?? "") ?? [];
  if (!server || !tool) return undefined;
  const words = server.replace(/[_-]/g, " ");
  return {
    server: server === "plxd" ? server : words.charAt(0).toUpperCase() + words.slice(1),
    tool: tool.replaceAll("_", " "),
  };
}

/** The skill a Skill call runs: `skill`, or `command` from older Claude Code versions. */
const skillName = (input?: JsonValue) => toolHint(input, ["skill", "command"]);

/**
 * A tool as a permission request names it (RYA-196): its kind's icon, then its name and what it
 * acts on, as its row would, or an MCP server's tool by the server, plxd's too. It hasn't run
 * yet, so a plxd tool isn't named by what it did.
 */
export function describeTool(name: string, input?: JsonValue): ToolLook {
  const item = { kind: "tool", key: "", callId: "", name, input } as const;
  const named = namedTool(item);
  return {
    Icon: icons[toolKind(item)],
    label: named?.label ?? name,
    detail: named ? named.detail : toolHint(input),
  };
}

/**
 * A tool call its row names readably: an MCP server's tool by the server, including a plxd tool
 * this app doesn't know, or a skill.
 */
function namedTool(item: Extract<Item, { kind: "tool" }>) {
  const mcp = mcpTool(item.name);
  if (mcp) return { label: mcp.server, detail: mcp.tool };
  if (item.name === "Skill") return { label: "Skill", detail: skillName(item.input) };
  return undefined;
}

// Tools whose input reads better as a permission request shows it: an edit's diff.
const previewed = new Set(["Edit", "MultiEdit", "Write"]);

/** Whether a tool's input arrived whole, not cut for size. */
const isWhole = (input?: JsonValue): input is JsonValue =>
  input !== undefined &&
  !(input && typeof input === "object" && !Array.isArray(input) && input["truncated"] === true);

function inputText(input: JsonValue): string {
  if (input && typeof input === "object" && !Array.isArray(input) && input["truncated"] === true) {
    const bytes = typeof input["bytes"] === "number" ? input["bytes"] : 0;
    return `Too large to show (${Math.ceil(bytes / 1024)} KB)`;
  }
  return typeof input === "string" ? input : JSON.stringify(input, null, 2);
}

/** The icon before a web link, as T3 Code shows one: its site's logo, or a globe (PLX-330). */
export function linkIcon(href?: string) {
  let host;
  try {
    const url = new URL(href ?? "");
    if (url.protocol !== "https:" && url.protocol !== "http:") return undefined;
    host = url.hostname;
  } catch {
    return undefined;
  }
  const on = (site: string) => host === site || host.endsWith(`.${site}`);
  return on("github.com") ? GitHubLogo : on("linear.app") ? LinearLogo : Globe;
}

// Agent output is untrusted: no raw HTML (no rehype-raw), and react-markdown's
// default urlTransform drops javascript: and other unsafe links. Links open in
// a new window, which main hands to the system browser, https only.
const markdownComponents: Components = {
  a: ({ href, children }) => {
    const Icon = linkIcon(href);
    return (
      <a href={href} target="_blank" rel="noreferrer">
        {Icon && (
          <Icon aria-hidden className="mr-1 inline size-3.5 align-[-0.15em] text-foreground" />
        )}
        {children}
      </a>
    );
  },
  pre: ({ node, children }) => {
    const code = node?.children[0];
    const classes = code?.type === "element" ? code.properties["className"] : undefined;
    const language = Array.isArray(classes)
      ? classes
          .map(String)
          .find((c) => c.startsWith("language-"))
          ?.slice("language-".length)
      : undefined;
    return (
      <CodeBlock language={language} text={code ? textOf(code) : ""}>
        {children}
      </CodeBlock>
    );
  },
  // Never load images: a link with the alt text, which opens externally like any link.
  img: ({ src, alt }) => (
    // An unsafe source arrives as "" from urlTransform: no href at all, then.
    <a href={typeof src === "string" && src ? src : undefined} target="_blank" rel="noreferrer">
      {alt || "Image"}
    </a>
  ),
};

// Highlights a code block whose fence names its language, never a guess; a diff's lines are
// drawn by DiffLines instead.
const highlight: ComponentProps<typeof Markdown>["rehypePlugins"] = [
  [rehypeHighlight, { detect: false, plainText: ["diff", "patch"] }],
];

/** A Markdown tree node's text, as a code block's, for Copy. */
type TextNode = { value?: string; children?: TextNode[] };
const textOf = (node: TextNode): string => node.value ?? node.children?.map(textOf).join("") ?? "";

/**
 * An agent message, rendered from Markdown with GitHub's extensions. `components` replace some
 * elements' renderers, such as the Context view's links; they get the same safe, HTML-free tree.
 */
export function MarkdownText({ text, components }: { text: string; components?: Components }) {
  return (
    <div className="markdown">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={highlight}
        components={{ ...markdownComponents, ...components }}
      >
        {text}
      </Markdown>
    </div>
  );
}

/** Copies text to the clipboard. `copied` is true for a moment after, for a Copied check. */
export function useCopy() {
  const [copied, setCopied] = useState(false);
  const copy = (text: string) =>
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  return [copied, copy] as const;
}

/** Under a prompt, on hover or focus: when it was sent, and Copy for its text. */
function PromptMeta({ at, text }: { at?: string; text?: string | null }) {
  const [copied, copy] = useCopy();
  return (
    <div className="flex h-6 items-center gap-1 text-[12px] text-faint-foreground opacity-0 group-focus-within/prompt:opacity-100 group-hover/prompt:opacity-100">
      {at && <time dateTime={at}>{sentAt(at)}</time>}
      {text && (
        <button
          type="button"
          aria-label={copied ? "Copied" : "Copy message"}
          onClick={() => copy(text)}
          className="grid size-6 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-3.5"
        >
          {copied ? <Check /> : <Copy />}
        </button>
      )}
    </div>
  );
}

/** "3:04 PM" today, and "Sep 25, 3:04 PM" before. */
function sentAt(at: string) {
  const date = new Date(at);
  const time: Intl.DateTimeFormatOptions = { hour: "numeric", minute: "2-digit" };
  return date.toDateString() === new Date().toDateString()
    ? date.toLocaleTimeString([], time)
    : date.toLocaleString([], { month: "short", day: "numeric", ...time });
}

/**
 * A Markdown code block: a header with its fence's language and Copy, over its code, highlighted,
 * or a diff's lines.
 */
function CodeBlock({
  language,
  text,
  children,
}: {
  language?: string;
  text: string;
  children: ReactNode;
}) {
  const [copied, copy] = useCopy();
  return (
    <div className="overflow-hidden rounded-xl border border-border bg-code">
      <div className="flex h-8 items-center justify-between border-b border-border pr-1 pl-3 text-[11.5px] text-faint-foreground">
        <span className="font-mono">{language ?? "text"}</span>
        <button
          type="button"
          aria-label={copied ? "Copied" : "Copy code"}
          onClick={() => copy(text)}
          className="flex h-6 items-center gap-1 rounded-md px-1.5 hover:bg-hover hover:text-foreground [&_svg]:size-3.5"
        >
          {copied ? <Check /> : <Copy />}
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      {language === "diff" || language === "patch" ? (
        <DiffLines text={text} />
      ) : (
        <pre className="code-lines overflow-x-auto px-3.5 py-3 font-mono text-[12.5px] leading-relaxed">
          {children}
        </pre>
      )}
    </div>
  );
}

/**
 * A unified diff's lines, as a permission request draws an edit. Hunk headers, and the file headers
 * before a file's first hunk, are bands, so a removed `-- comment` inside a hunk stays removed.
 */
function DiffLines({ text }: { text: string }) {
  let inHunk = false;
  return (
    <div className="code-scroll overflow-x-auto py-1.5 font-mono text-[12px] leading-relaxed">
      {text
        .replace(/\n$/, "")
        .split("\n")
        .map((line, i) => {
          if (line.startsWith("diff ")) inHunk = false;
          if (line.startsWith("@@")) inHunk = true;
          const op = line[0];
          if (line.startsWith("@@") || (!inHunk && /^(\+\+\+|---|diff |index )/.test(line)))
            return (
              <div key={i} className={`${diffBand} whitespace-pre-wrap`}>
                {line}
              </div>
            );
          return op === "+" || op === "-" || op === " " ? (
            <DiffRow key={i} op={op} text={line.slice(1)} />
          ) : (
            <DiffRow key={i} op=" " text={line} />
          );
        })}
    </div>
  );
}

/**
 * An open run in the composer's tab: that it runs in a worktree, and the worktree's branch, or in
 * the repository's own checkout, with `children`, such as Open PR, before the branch.
 */
export function RunTab({ run, children }: { run: AgentRun; children?: ReactNode }) {
  return (
    <>
      {run.checkout ? (
        <span className={`${tabItem} shrink-0`}>
          <Folder aria-hidden />
          Current checkout
        </span>
      ) : (
        <span className={`${tabItem} shrink-0`}>
          <FolderGit2 aria-hidden />
          Worktree
        </span>
      )}
      <span className="flex min-w-0 items-center">
        {children}
        {run.branch && (
          <span className={tabItem} title={`Worktree branch: ${run.branch}`}>
            <GitBranch aria-hidden />
            <span className="truncate">{run.branch}</span>
          </span>
        )}
      </span>
    </>
  );
}

/**
 * Open PR: plxd pushes the run's branch and opens a pull request titled like the thread, then
 * this links to it, in the browser, or hands it to `onOpened`. It unmounts while the run works, so
 * after another turn the button is back, to push the new commit to the same pull request.
 */
function OpenPr({
  hostId,
  run,
  onError,
  onOpened,
}: {
  hostId: string;
  run: AgentRun;
  onError: (error?: RpcError) => void;
  onOpened?: (url: string) => void;
}) {
  const [url, setUrl] = useState<string>();
  const [opening, setOpening] = useState(false);
  if (url) {
    const number = /\/pull\/(\d+)$/.exec(url)?.[1];
    return (
      <a
        href={url}
        target="_blank"
        rel="noreferrer"
        title={url}
        className={`${tabItem} rounded-md hover:bg-hover hover:text-foreground`}
      >
        <GitPullRequest aria-hidden />
        {number ? `PR #${number}` : "Pull request"}
      </a>
    );
  }
  const open = async () => {
    setOpening(true);
    onError(undefined);
    const answer = await window.parallax.request(hostId, "agent/openPr", {
      runId: run.id,
      title: titleOf(run),
    });
    setOpening(false);
    if ("error" in answer) onError(answer.error);
    else if (onOpened) onOpened(answer.result.url);
    else setUrl(answer.result.url);
  };
  return (
    <button
      type="button"
      disabled={opening}
      onClick={() => void open()}
      className={`${tabItem} rounded-md enabled:hover:bg-hover enabled:hover:text-foreground disabled:opacity-60`}
    >
      <GitPullRequestArrow aria-hidden />
      {opening ? "Opening PR…" : "Open PR"}
    </button>
  );
}
