import { useCallback, useEffect, useState } from "react";

import type { RpcError } from "../preload/bridge";
import type { AgentSendParams, PromptImage } from "../protocol/generated/protocol";
import { applyEvents, emptyTranscript, isRunning, type Transcript } from "./transcript";
import { uuidv7 } from "./uuidv7";

export interface AgentRunView {
  transcript: Transcript;
  /** Why the transcript couldn't load, for people. */
  error?: string;
  /** Messages this window sent, by turn id, since older logs hold only the id (RYA-92). */
  sent: ReadonlyMap<string, SentMessage>;
  /**
   * Sends a message, its images, and the threads attached to it as the run's next turn, with a new
   * model, effort, or access for the run if given. Resolves to plxd's error, or why the run
   * couldn't take it, or undefined.
   */
  send: (
    text: string,
    options?: SendOptions,
    images?: PromptImage[],
    threads?: string[],
  ) => Promise<RpcError | undefined>;
  /** Stops the run. Resolves to an error message, or undefined. */
  cancel: () => Promise<string | undefined>;
}

/**
 * A message this window sent: its text, images, and attached threads' run ids, at hand until the
 * run's log has them.
 */
export interface SentMessage {
  text: string;
  images: PromptImage[];
  threads?: string[];
}

/**
 * A new model, effort, or access for a run, sent only to a plxd that advertises `sendModel`, and
 * a new account, perhaps another provider's, only to one that advertises `sendAccount`.
 */
export type SendOptions = Pick<
  AgentSendParams,
  "model" | "effort" | "permission" | "contextWindow" | "fast" | "account"
>;

/**
 * One run's transcript, kept live: pages through `agent/events`, then subscribes
 * to its scope's events from a fresh snapshot `seq`, and starts over on `resync`.
 * Loads only while `connected`; a reconnect loads again. Key the caller by host
 * and run, so another run starts from an empty transcript.
 */
export function useAgentRun(hostId: string, runId: string, connected: boolean): AgentRunView {
  const [transcript, setTranscript] = useState(emptyTranscript);
  const [error, setError] = useState<string>();
  const [sent, setSent] = useState<ReadonlyMap<string, SentMessage>>(new Map());

  useEffect(() => {
    if (!connected) return;
    let stopped = false;
    let unsubscribe = () => {};

    async function load() {
      let t = emptyTranscript;
      // The scope's seq from `agent/list`, taken before the last page is read. The run's
      // own last seq can be too old for plxd to replay the scope from, which would
      // answer every subscribe with another resync. Subscribing after the snapshot is
      // gap-free, since at least one page was read after it.
      let snapshot: number | undefined;
      // The log the first page was read under. If it changes before the subscribe, main
      // answers it with a resync, so a mix of two logs' seqs is never used.
      let logId: string | undefined;
      for (let more = true; more || snapshot === undefined;) {
        // agent.started, the first event, carries the run's scope: its project, or its
        // thread's repo entry (0017).
        if (snapshot === undefined && t.run) {
          const list = await window.parallax.request(hostId, "agent/list", {
            project: t.run.project,
          });
          if (stopped) return;
          if ("error" in list) return setError(list.error.message);
          snapshot = list.result.seq;
        }
        const page = await window.parallax.request(hostId, "agent/events", { runId, after: t.seq });
        if (stopped) return;
        if ("error" in page) return setError(page.error.message);
        logId ??= page.logId;
        t = applyEvents(t, page.result.events, runId);
        more = page.result.more;
        if (!more && !t.run) return setError("This agent run hasn't started.");
      }
      // The loop only ends with both, but the types can't tell.
      if (!t.run || snapshot === undefined || logId === undefined) return;
      setTranscript(t);
      setError(undefined);
      unsubscribe = window.parallax.subscribe(
        hostId,
        { after: Math.max(t.seq, snapshot), project: t.run.project, logId },
        (message) => {
          if (stopped) return;
          if (message.type === "event")
            setTranscript((prev) => applyEvents(prev, [message.event], runId));
          else if (message.type === "resync") void load();
          else setError(message.error.message);
        },
      );
    }

    void load();
    return () => {
      stopped = true;
      unsubscribe();
    };
  }, [hostId, runId, connected]);

  const send = useCallback(
    async (
      text: string,
      options?: SendOptions,
      images: PromptImage[] = [],
      threads: string[] = [],
    ) => {
      const turnId = uuidv7();
      setSent((prev) => new Map(prev).set(turnId, { text, images, threads }));
      const answer = await window.parallax.request(hostId, "agent/send", {
        runId,
        turnId,
        text,
        ...options,
        ...(images.length > 0 && { images }),
        ...(threads.length > 0 && { threads }),
      });
      if ("result" in answer && isRunning(answer.result.run.status)) return undefined;
      setSent((prev) => {
        const next = new Map(prev);
        next.delete(turnId);
        return next;
      });
      // A finished run whose CLI didn't start again answers with the failed run, and no turn
      // follows for the message.
      return "error" in answer
        ? answer.error
        : { code: -32000, message: answer.result.run.error ?? "The agent couldn't start." };
    },
    [hostId, runId],
  );

  const cancel = useCallback(async () => {
    const answer = await window.parallax.request(hostId, "agent/cancel", { runId });
    return "error" in answer ? answer.error.message : undefined;
  }, [hostId, runId]);

  return { transcript, error, sent, send, cancel };
}
