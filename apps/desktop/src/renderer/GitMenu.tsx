import {
  ChevronDown,
  CloudUpload,
  GitCommitHorizontal,
  GitPullRequestArrow,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState, type ToggleEvent } from "react";

import type { RpcError } from "../preload/bridge";
import type { AgentRun, GitStatus } from "../protocol/generated/protocol";
import { useConnection } from "./ConnectionStatus";
import { describeError } from "./errors";
import { titleOf } from "./threads";
import { isRunning } from "./transcript";
import { menuButton, menuItem, menuPanel, moveFocus } from "./ui";

export type GitAction = "commit" | "push" | "pr";

const actions: { action: GitAction; label: string; busy: string; Icon: LucideIcon }[] = [
  { action: "commit", label: "Commit", busy: "Committing…", Icon: GitCommitHorizontal },
  { action: "push", label: "Push", busy: "Pushing…", Icon: CloudUpload },
  { action: "pr", label: "Create PR", busy: "Creating PR…", Icon: GitPullRequestArrow },
];

/**
 * What the Git menu offers a run whose folder is in `status`: why each action can't run (absent
 * when it can), the main one (Commit with changes, else Push with unpushed commits, else Create
 * PR), and a note under the actions, as for a detached HEAD.
 */
export function gitActions(
  run: AgentRun,
  status: GitStatus | undefined,
  canOpenPr: boolean,
): { why: Partial<Record<GitAction, string>>; main: GitAction; note?: string } {
  const all = (reason: string) => ({ commit: reason, push: reason, pr: reason });
  if (isRunning(run.status)) return { why: all("Wait for the turn to end"), main: "commit" };
  if (!status) return { why: all("Loading…"), main: "commit" };
  const why: Partial<Record<GitAction, string>> = {};
  let note: string | undefined;
  if (status.changes === 0) why.commit = "No changes";
  if (!status.origin) why.push = why.pr = "No origin remote";
  else if (!status.branch) {
    why.push = why.pr = "Detached HEAD";
    note = "Detached HEAD: check out a branch to push or open a pull request.";
  } else {
    if (status.ahead === 0) why.push = "Nothing to push";
    if (!canOpenPr) why.pr = "This host's plxd can't open pull requests";
    // A worktree's pull request is of the run's commits.
    else if (!run.checkout && !run.diff) why.pr = "No commits yet";
  }
  const main = !why.commit ? "commit" : !why.push ? "push" : "pr";
  return { why, main, note };
}

/**
 * The top bar's Git split button for a thread (RYA-298). The main part runs the next useful
 * action; the chevron lists Commit, Push, and Create PR, each disabled with its reason. Commit
 * asks for a message, prefilled with the thread's title. Create PR pushes and opens the pull
 * request through `agent/openPr`, in the browser, or with `onPrOpened`, in the PR view. The status
 * is read again after each action and whenever the run's status or commit changes, as when a turn
 * ends. Absent until the host's plxd has the `git` capability.
 */
export function GitMenu({
  hostId,
  run,
  title,
  onPrOpened,
}: {
  hostId: string;
  run?: AgentRun;
  /** The thread's title, for the commit message and pull request. Else, its run's. */
  title?: string;
  onPrOpened?: (url: string) => void;
}) {
  const id = useId();
  const menu = useRef<HTMLDivElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const connection = useConnection(hostId);
  const capabilities = connection?.status === "connected" ? connection.capabilities : undefined;
  const able = !!capabilities && "git" in capabilities;
  const [status, setStatus] = useState<GitStatus>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState<GitAction>();
  const [message, setMessage] = useState("");
  const runId = run?.id;

  const load = useCallback(
    async (live: () => boolean = () => true) => {
      if (!runId) return;
      const answer = await window.parallax.request(hostId, "agent/gitStatus", { runId });
      if (!live()) return;
      if ("error" in answer) setError(describeError(answer.error));
      else setStatus(answer.result);
    },
    [hostId, runId],
  );
  const runStatus = run?.status;
  const commit = run?.diff?.commit;
  useEffect(() => {
    if (!able) return;
    let live = true;
    void load(() => live);
    return () => {
      live = false;
    };
  }, [able, load, runStatus, commit]);

  if (!able || !run) return null;
  const { why, main, note } = gitActions(run, status, "openPr" in capabilities);
  const primary = actions.find((a) => a.action === main)!;

  // Runs `action`; a failure shows in the menu, opened to say so, or in the commit dialog.
  const perform = async (action: GitAction) => {
    setBusy(action);
    setError(undefined);
    let failed: RpcError | undefined;
    if (action === "pr") {
      const answer = await window.parallax.request(hostId, "agent/openPr", {
        runId: run.id,
        title: title ?? titleOf(run),
      });
      if ("error" in answer) failed = answer.error;
      else {
        if (onPrOpened) onPrOpened(answer.result.url);
        else window.open(answer.result.url, "_blank");
        await load();
      }
    } else {
      const answer =
        action === "commit"
          ? await window.parallax.request(hostId, "agent/commit", { runId: run.id, message })
          : await window.parallax.request(hostId, "agent/push", { runId: run.id });
      if ("error" in answer) failed = answer.error;
      else setStatus(answer.result);
    }
    setBusy(undefined);
    if (failed) setError(describeError(failed));
    if (action === "commit" && !failed) dialog.current?.close();
    if (action !== "commit" && failed) menu.current?.showPopover();
  };
  const start = (action: GitAction) => {
    menu.current?.hidePopover();
    if (action !== "commit") return void perform(action);
    setError(undefined);
    setMessage(title ?? titleOf(run));
    dialog.current?.showModal();
  };

  return (
    <>
      <div className="flex shrink-0 items-center rounded-lg border border-border">
        <button
          type="button"
          disabled={!!why[main] || !!busy}
          onClick={() => start(main)}
          title={why[main]}
          className={`${menuButton} rounded-r-none pr-2.5`}
        >
          <primary.Icon aria-hidden />
          {busy ? actions.find((a) => a.action === busy)!.busy : primary.label}
        </button>
        <button
          type="button"
          popoverTarget={id}
          aria-haspopup="menu"
          aria-label="Git actions"
          className={`${menuButton} self-stretch rounded-l-none border-l border-border pr-1 pl-1`}
        >
          <ChevronDown aria-hidden className="opacity-70" />
        </button>
      </div>
      <div
        ref={menu}
        id={id}
        popover="auto"
        role="menu"
        aria-label="Git"
        onToggle={(e: ToggleEvent<HTMLDivElement>) => {
          if (e.newState === "open")
            e.currentTarget.querySelector<HTMLElement>("button:enabled")?.focus();
        }}
        onKeyDown={moveFocus}
        className={`${menuPanel("end")} w-80 p-1`}
      >
        {actions.map(({ action, label, Icon }) => (
          <button
            key={action}
            type="button"
            role="menuitem"
            disabled={!!why[action] || !!busy}
            onClick={() => start(action)}
            className={`${menuItem} disabled:opacity-50 disabled:hover:bg-transparent [&_svg]:size-4`}
          >
            <Icon aria-hidden />
            <span className="flex-1">{label}</span>
            {why[action] && (
              <span className="text-[11.5px] text-faint-foreground">{why[action]}</span>
            )}
          </button>
        ))}
        {note && <p className="px-2 pt-1 pb-1.5 text-[12px] text-amber-500">{note}</p>}
        {error && (
          <p role="alert" className="px-2 pt-1 pb-1.5 text-[12px] text-danger">
            {error}
          </p>
        )}
      </div>
      <dialog
        ref={dialog}
        aria-labelledby={`${id}-commit`}
        className="m-auto w-[28rem] rounded-xl border border-border bg-surface text-foreground shadow-composer backdrop:bg-black/50"
      >
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void perform("commit");
          }}
        >
          <div className="px-5 pt-4 pb-3">
            <h2 id={`${id}-commit`} className="text-[15px] font-semibold">
              Commit changes
            </h2>
            <p className="mt-0.5 text-[12.5px] text-muted-foreground">
              Stages every change on {status?.branch ?? "the detached HEAD"} and commits it.
            </p>
          </div>
          <div className="px-5 pb-4">
            <textarea
              aria-label="Commit message"
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              rows={3}
              spellCheck={false}
              className="w-full resize-none rounded-lg border border-border bg-transparent px-3 py-2 text-[13px] focus-visible:outline-2 focus-visible:outline-ring"
            />
          </div>
          <div className="flex items-center gap-3 border-t border-border px-5 py-3">
            {error && (
              <p role="alert" className="min-w-0 text-[12px] text-danger">
                {error}
              </p>
            )}
            <button
              type="button"
              onClick={() => dialog.current?.close()}
              className="ml-auto shrink-0 rounded-md px-3 py-1.5 text-[13px] text-muted-foreground hover:bg-hover hover:text-foreground"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={!message.trim() || !!busy}
              className="shrink-0 rounded-md bg-primary px-3 py-1.5 text-[13px] font-medium text-primary-foreground enabled:hover:opacity-90 disabled:opacity-50"
            >
              {busy === "commit" ? "Committing…" : "Commit"}
            </button>
          </div>
        </form>
      </dialog>
    </>
  );
}
