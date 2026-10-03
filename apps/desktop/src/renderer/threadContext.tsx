// Threads attached to a message as context (PLX-378, decision 0047): how one looks as a chip, in
// the composer and in the sent message, and what attaching takes.
import { MessagesSquare, X } from "lucide-react";
import { createContext, useContext, type ComponentType, type SVGProps } from "react";

import type { ConnectionState } from "../preload/bridge";
import { lastPrompt } from "./attention";
import { age, backendLogos } from "./Sidebar";
import type { ThreadsState } from "./threads";

/** The open host's threads, for each one's title, provider, and time, and how to open one. */
export interface ThreadLinks {
  hostId: string;
  state: ThreadsState;
  open: (runId: string) => void;
}

/** What the composer needs to attach threads: the most a message takes, and the open thread, which can't attach itself. */
export interface AttachThreads extends ThreadLinks {
  max: number;
  self?: string;
}

/**
 * `links` for the composer, on a host whose plxd takes threads with a message (`threadContext`).
 * Undefined otherwise, which leaves the composer without them.
 */
export function attachThreads(
  connection: ConnectionState | undefined,
  links: ThreadLinks | undefined,
  self?: string,
): AttachThreads | undefined {
  const options =
    connection?.status === "connected" ? connection.capabilities["threadContext"] : undefined;
  const max = options?.["maxThreads"];
  return links && typeof max === "number" ? { ...links, max, self } : undefined;
}

/** The open host's threads, for the transcript's chips, which open the thread they name. */
export const ThreadLinksContext = createContext<ThreadLinks | undefined>(undefined);

/** How a thread's chip and `@` row show it. `known` is false for one the host doesn't list. */
export interface ThreadLook {
  title: string;
  Logo: ComponentType<SVGProps<SVGSVGElement>>;
  /** How long ago its newest message was sent, as the sidebar writes it: "3h". */
  age?: string;
  known: boolean;
}

/** Thread `runId` as the sidebar shows it: its title, its provider's logo, and when it was last prompted. */
export function lookOf(state: ThreadsState | undefined, runId: string): ThreadLook {
  const thread = state?.threads.find((t) => t.id === runId);
  const backend = state?.runs[runId]?.backend;
  return {
    title: state?.titles[runId] ?? thread?.title ?? (thread ? "Untitled" : "Unavailable"),
    Logo: (backend && backendLogos[backend]) || MessagesSquare,
    age: thread && age(lastPrompt(thread)),
    known: !!thread,
  };
}

/**
 * An attached thread, the size of the composer's image thumbnails: its provider, how long ago it
 * was prompted, and its title. With `onOpen` it's a button that opens the thread; with `onRemove`
 * it has a remove button in its corner, as an image does.
 */
export function ThreadChip({
  look,
  onOpen,
  onRemove,
}: {
  look: ThreadLook;
  onOpen?: () => void;
  onRemove?: () => void;
}) {
  const body = (
    <>
      <span className="flex items-center gap-1.5 text-[11.5px] text-faint-foreground">
        <look.Logo aria-hidden className="size-3.5 shrink-0" />
        {look.age ? `Thread · ${look.age}` : "Thread"}
      </span>
      <span className="w-full truncate text-[12.5px] text-foreground">{look.title}</span>
    </>
  );
  const box =
    "flex h-14 w-48 flex-col items-start justify-center gap-1 rounded-xl border border-border bg-surface px-3 text-left";
  return (
    <span className="relative" data-thread-chip>
      {onOpen ? (
        <button
          type="button"
          title={`Open ${look.title}`}
          onClick={onOpen}
          className={`${box} hover:bg-hover`}
        >
          {body}
        </button>
      ) : (
        <span className={box}>{body}</span>
      )}
      {onRemove && (
        <button
          type="button"
          aria-label={`Remove ${look.title}`}
          onClick={onRemove}
          className="absolute -top-1.5 -right-1.5 grid size-5 place-items-center rounded-full border border-border bg-surface text-muted-foreground shadow-sm hover:text-foreground"
        >
          <X className="size-3" />
        </button>
      )}
    </span>
  );
}

/** A thread a sent message carried, as a chip that opens it while the host lists it. */
export function SentThread({ runId }: { runId: string }) {
  const links = useContext(ThreadLinksContext);
  const look = lookOf(links?.state, runId);
  return (
    <ThreadChip look={look} onOpen={links && look.known ? () => links.open(runId) : undefined} />
  );
}
