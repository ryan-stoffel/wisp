import { Bot } from "lucide-react";
import { useEffect, useId, useRef, useState, type RefObject, type ToggleEvent } from "react";

import type { AgentRun, Thread } from "../protocol/generated/protocol";
import { attentionOf, type Attention } from "./attention";
import { duration } from "./AttentionMark";
import { models } from "./models";
import { age, backendLogos } from "./Sidebar";
import { asksOf, childrenOf, type ThreadsState } from "./threads";
import { isRunning, statusLabel } from "./transcript";
import { menuPanel } from "./ui";

// How many chips the top bar shows before the rest go behind +N, and the room each takes at
// most (a chip is at most 9rem, plus its gap), and +N's.
const maxChips = 4;
const chipRoom = 146;
const moreRoom = 40;

/** An element's width, kept current. Infinite where nothing lays it out, as in tests. */
function useWidth(ref: RefObject<HTMLElement | null>) {
  const [width, setWidth] = useState(Number.POSITIVE_INFINITY);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry && entry.contentRect.width > 0) setWidth(entry.contentRect.width);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

const dotColors: Record<Attention, string> = {
  working: "bg-working motion-safe:animate-pulse",
  needsYou: "bg-warning",
  done: "bg-added",
  failed: "bg-danger",
  settled: "bg-faint-foreground/45",
};

const attentionNames: Record<Exclude<Attention, "settled">, string> = {
  working: "Working",
  needsYou: "Needs you",
  done: "Done",
  failed: "Failed",
};

/** A thread's status (0033) as a small dot: Working pulses, and settled is a quiet gray. */
export function StatusDot({ attention }: { attention: Attention }) {
  return <span aria-hidden className={`size-1.5 shrink-0 rounded-full ${dotColors[attention]}`} />;
}

/** A run's provider logo, or a plain bot for a backend this app has no logo for. */
function ProviderLogo({ run, className }: { run?: AgentRun; className: string }) {
  const Logo = (run && backendLogos[run.backend]) ?? Bot;
  return <Logo aria-hidden className={`shrink-0 ${className}`} />;
}

/** What a thread is doing, for people: its attention, or once settled, its run's status. */
function statusText(attention: Attention, run?: AgentRun) {
  if (attention !== "settled") return attentionNames[attention];
  return run ? statusLabel(run.status) : "Idle";
}

/**
 * The top bar's chips for a thread's lineage (0041), after its title crumb: one per child, or as
 * a child, one per sibling with its own underlined. Each is a status dot, the provider's logo,
 * and the title cut short. At most four show, the open one always among them, and `+N` opens the
 * whole tree from `root`.
 */
export function LineageTrail({
  state,
  chips,
  active,
  root,
  openId,
  onOpen,
}: {
  state: ThreadsState;
  chips: Thread[];
  /** The open thread, when it is one of the chips. */
  active?: string;
  /** The tree's top thread. */
  root: Thread;
  /** The open thread, marked in the tree. */
  openId: string;
  onOpen: (threadId: string) => void;
}) {
  const group = useRef<HTMLDivElement>(null);
  const width = useWidth(group);
  // As many as fit, up to four, and at least one; +N takes its room once any are left out.
  const fit = (room: number) => Math.floor(room / chipRoom);
  const count = Math.max(
    1,
    Math.min(maxChips, fit(width) >= chips.length ? chips.length : fit(width - moreRoom)),
  );
  let shown = chips.slice(0, count);
  const current = chips.find((c) => c.id === active);
  if (current && !shown.includes(current)) shown = [...chips.slice(0, count - 1), current];
  const more = chips.length - shown.length;
  const attention = (t: Thread) => attentionOf(t, state.runs[t.id], asksOf(state, t.id));
  return (
    <div
      ref={group}
      role="group"
      aria-label={active ? "Sibling threads" : "Child threads"}
      className="flex min-w-0 flex-1 items-center gap-0.5 overflow-hidden"
    >
      {shown.map((t) => {
        const title = state.titles[t.id] ?? "Thread";
        const a = attention(t);
        const on = t.id === active;
        return (
          <button
            key={t.id}
            type="button"
            aria-current={on ? "page" : undefined}
            title={`${title} · ${statusText(a, state.runs[t.id])}`}
            onClick={() => onOpen(t.id)}
            className={`relative flex max-w-36 min-w-0 items-center gap-1.5 rounded-md px-1.5 py-1 focus-visible:-outline-offset-2 ${on ? "text-foreground after:absolute after:inset-x-1.5 after:bottom-0 after:h-[1.5px] after:rounded-full after:bg-foreground/55" : "text-muted-foreground hover:text-foreground"}`}
          >
            <StatusDot attention={a} />
            <ProviderLogo run={state.runs[t.id]} className="size-3.5" />
            <span data-chip-title className="truncate">
              {title}
            </span>
          </button>
        );
      })}
      {more > 0 && (
        <LineageTree state={state} root={root} openId={openId} more={more} onOpen={onOpen} />
      )}
    </div>
  );
}

/** `Date.now()`, again each second while `on`. */
function useNow(on: boolean) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!on) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [on]);
  return now;
}

/**
 * `+N`, which opens the whole tree under `root` in a native popover: each thread's status, title,
 * model, status, how long it has run, and when it was last active. A row opens its thread.
 */
function LineageTree({
  state,
  root,
  openId,
  more,
  onOpen,
}: {
  state: ThreadsState;
  root: Thread;
  openId: string;
  more: number;
  onOpen: (threadId: string) => void;
}) {
  const id = useId();
  const panel = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const now = useNow(open);

  // Depth first, each thread's children in launch order. A thread listed twice stops there.
  const rows: { thread: Thread; depth: number }[] = [];
  const seen = new Set<string>();
  const walk = (thread: Thread, depth: number) => {
    if (seen.has(thread.id)) return;
    seen.add(thread.id);
    rows.push({ thread, depth });
    for (const child of childrenOf(state, thread.id)) walk(child, depth + 1);
  };
  walk(root, 0);

  return (
    <>
      <button
        type="button"
        popoverTarget={id}
        aria-haspopup="dialog"
        aria-label={`${more} more threads`}
        title="All threads in this tree"
        className="shrink-0 rounded-md px-1.5 py-1 text-[12.5px] focus-visible:-outline-offset-2 text-muted-foreground tabular-nums hover:bg-hover hover:text-foreground"
      >
        +{more}
      </button>
      <div
        ref={panel}
        id={id}
        popover="auto"
        role="dialog"
        aria-label="Thread tree"
        onToggle={(e: ToggleEvent<HTMLDivElement>) => setOpen(e.newState === "open")}
        className={`${menuPanel("start")} w-84 p-1`}
      >
        <ul className="flex max-h-96 flex-col gap-px overflow-y-auto">
          {rows.map(({ thread, depth }) => {
            const run = state.runs[thread.id];
            const attention = attentionOf(thread, run, asksOf(state, thread.id));
            const model = run?.model && (models.find((m) => m.id === run.model)?.name ?? run.model);
            const end = run && (isRunning(run.status) ? now : Date.parse(run.updatedAt));
            const seconds = run && end ? Math.floor((end - Date.parse(run.createdAt)) / 1000) : 0;
            const last = run && age(run.updatedAt, now);
            const current = thread.id === openId;
            return (
              <li key={thread.id}>
                <button
                  type="button"
                  aria-current={current ? "page" : undefined}
                  onClick={() => {
                    panel.current?.hidePopover();
                    onOpen(thread.id);
                  }}
                  style={{ paddingLeft: `${8 + depth * 14}px` }}
                  className={`flex w-full flex-col gap-0.5 rounded-md py-1.5 pr-2 text-left hover:bg-hover ${current ? "bg-selected" : ""}`}
                >
                  <span className="flex w-full min-w-0 items-center gap-2 text-[13px]">
                    <StatusDot attention={attention} />
                    <ProviderLogo run={run} className="size-3.5" />
                    <span className="min-w-0 flex-1 truncate text-foreground">
                      {state.titles[thread.id] ?? "Thread"}
                    </span>
                    {run && (
                      <span className="shrink-0 text-[11.5px] text-faint-foreground tabular-nums">
                        {duration(Math.max(0, seconds))}
                      </span>
                    )}
                  </span>
                  {/* Under the title: past the dot and logo. */}
                  <span className="block w-full truncate pl-9 text-[11.5px] text-faint-foreground">
                    {[
                      model,
                      statusText(attention, run),
                      last && (last === "now" ? "just now" : `${last} ago`),
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}
