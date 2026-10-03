import { Folder, House, LoaderCircle, Plus } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import type { RpcError, ThreadName } from "../preload/bridge";
import type { AccountChoice, PromptImage, Repo, Role } from "../protocol/generated/protocol";
import { TranscriptView } from "./AgentChat";
import { Composer, tabItem } from "./Composer";
import { useConnection } from "./ConnectionStatus";
import { describeError } from "./errors";
import type { Host } from "./hosts";
import { imageCaps } from "./images";
import type { RunOptions } from "./models";
import { RefMenu } from "./RefMenu";
import { RunTargetMenu, type Workspace } from "./RunTargetMenu";
import { attachThreads, ThreadLinksContext, type ThreadLinks } from "./threadContext";
import { noRepo, type ThreadGroup } from "./threads";
import { ariaKeyshortcut, bindingsOf, useShortcutLabel } from "./keybindings";
import { Picker } from "./ui";
import { uuidv7 } from "./uuidv7";

// The picker's value that opens the folder picker instead of choosing a group.
const addRepository = "add-repository";

interface NewThreadProps {
  hostId: string;
  /** The computers a thread can run on. */
  hosts: Host[];
  /** Repositories and No Repo, as the sidebar groups them. */
  groups: ThreadGroup[];
  groupId: string;
  onGroupChange: (groupId: string) => void;
  /** Whether the host is this computer, so its folders can be picked. */
  local: boolean;
  addRepo: (path: string) => Promise<Repo | string>;
  start: (
    runId: string,
    groupId: string,
    prompt: string,
    images: PromptImage[],
    options: RunOptions,
    checkout: boolean,
    gitRef: string | undefined,
    name?: ThreadName,
    attached?: string[],
  ) => Promise<RpcError | undefined>;
  /** Whether the host's plxd takes a thread's model, effort, and permission (`runOptions`). */
  runOptions: boolean;
  /**
   * Called once plxd has the thread, with a note for it, such as which account it picked.
   * `background` when Cmd/Ctrl+Enter started it, so New Thread stays open.
   */
  onStarted: (runId: string, notice: string | undefined, background: boolean) => void;
  disabledReason?: string;
  /** The host's threads, which the composer attaches where plxd takes them (PLX-378). */
  threadLinks?: ThreadLinks;
}

/** One start of a thread. Retrying it reuses its run id, so plxd never makes a second thread (0007). */
interface Attempt {
  runId: string;
  groupId: string;
  prompt: string;
  images: PromptImage[];
  /** The run ids of the threads attached to the prompt. */
  threads: string[];
  options: RunOptions;
  checkout: boolean;
  /** The picked ref: the worktree's base, or the branch the checkout switches to. */
  gitRef?: string;
  name: ThreadName;
  /** Started with Cmd/Ctrl+Enter, leaving New Thread open. */
  background: boolean;
}

/** An account a worker or coordinator can run on, as the account chooser lists it. */
export interface AccountOption {
  label: string;
  account: AccountChoice;
}

/**
 * The host's accounts a thread or a Project's coordinator can run on: its signed-in Claude Code
 * login, then its Anthropic key accounts. Resolves to plxd's error, for people, when either list
 * fails.
 */
export async function accountOptions(hostId: string): Promise<AccountOption[] | string> {
  const [clis, keys] = await Promise.all([
    window.parallax.request(hostId, "accounts/list", {}),
    window.parallax.request(hostId, "accounts/keys/list", {}),
  ]);
  if ("error" in clis) return describeError(clis.error);
  if ("error" in keys) return describeError(keys.error);
  // ponytail: mirrors plxd's backend registry, where only Claude runs workers today. RYA-99 has
  // plxd report which accounts can run a thread, so this stops hard-coding it.
  return [
    ...clis.result.clis
      .filter((c) => c.cli === "claude" && c.signedIn)
      .map((c) => ({
        label: "Claude Code",
        account: { kind: "subscription", backend: c.cli } as const,
      })),
    ...keys.result.accounts
      .filter((k) => k.provider === "anthropic")
      .map((k) => ({
        label: `${k.label} (API key)`,
        account: { kind: "key", id: k.id } as const,
      })),
  ];
}

/**
 * The backend the host's new runs in `role` run on: its default's, or Claude's with no default,
 * since only Claude accounts are offered (`accountOptions`). Undefined when that can't be told.
 */
export async function defaultBackend(hostId: string, role: Role): Promise<string | undefined> {
  const defaults = await window.parallax.request(hostId, "accounts/defaults/get", {});
  if ("error" in defaults) return undefined;
  const choice = defaults.result[role];
  if (!choice) return "claude";
  if (choice.kind === "subscription") return choice.backend;
  if (choice.kind !== "key") return undefined;
  const keys = await window.parallax.request(hostId, "accounts/keys/list", {});
  if ("error" in keys) return undefined;
  const provider = keys.result.accounts.find((k) => k.id === choice.id)?.provider;
  return provider === "anthropic" ? "claude" : provider === "openai" ? "codex" : undefined;
}

const noAccounts =
  "No account can run this thread yet. Sign in to Claude Code, or add an API key, then try again.";

/**
 * The New Thread screen: a centered composer, and under it where the thread runs. When the host
 * has no usable default account for threads, it picks the only one there is (and says so), or asks
 * which to use, sets it as the default, and retries the same start. plxd never picks one itself, so it never spends
 * a subscription the user didn't choose.
 */
export function NewThread({
  hostId,
  hosts,
  groups,
  groupId,
  onGroupChange,
  local,
  addRepo,
  start,
  runOptions,
  onStarted,
  disabledReason,
  threadLinks,
}: NewThreadProps) {
  // The no-repo start's shortcut, as Settings > Keybinds has it.
  const noRepoKeys = useShortcutLabel("noRepoThread");
  const noRepoBinding = bindingsOf("noRepoThread")[0];
  // ponytail: read as the screen opens, since plxd has no event for a changed default. One
  // changed elsewhere shows once New Thread opens again. Until then plxd refuses an effort or
  // permission the new backend can't run, but not the old backend's model: that run fails in the CLI.
  const [backend, setBackend] = useState<string>();
  useEffect(() => {
    if (!runOptions) return;
    let live = true;
    void defaultBackend(hostId, "worker").then((b) => live && setBackend(b));
    return () => {
      live = false;
    };
  }, [hostId, runOptions]);
  const connection = useConnection(hostId);
  const [repoError, setRepoError] = useState<string>();
  // Open while the host needs an account for the failed start.
  const [choices, setChoices] = useState<AccountOption[]>();
  const [picked, setPicked] = useState(0);
  const [choosing, setChoosing] = useState(false);
  const [chooseError, setChooseError] = useState<string>();
  // The prompt, images, and threads of a start in flight. Starting takes plxd a moment (a
  // worktree, a worker), so the screen shows them as the thread it opens meanwhile.
  const [starting, setStarting] = useState<{
    prompt: string;
    images: PromptImage[];
    threads: string[];
  }>();
  const failed = useRef<Attempt>(undefined);
  const group = groups.find((g) => g.id === groupId) ?? groups.at(-1)!;
  const [workspace, setWorkspace] = useState<Workspace>("worktree");
  // Only a repository has a checkout, and only a plxd that takes `checkout` would use it: an
  // older one would make a worktree anyway.
  const checkoutUnavailable =
    group.id === noRepo
      ? "needs a repo"
      : connection?.status === "connected" && !("checkout" in connection.capabilities)
        ? "needs a newer plxd"
        : undefined;
  const checkout = workspace === "checkout" && !checkoutUnavailable;
  // Only a plxd with `repoRefs` lists refs or starts from one, and only a repository has them.
  const refsAvailable =
    group.id !== noRepo &&
    connection?.status === "connected" &&
    "repoRefs" in connection.capabilities;
  // The picked ref, kept with its repository and workspace, so picking another drops it: a new
  // worktree's base never becomes a checkout's switch.
  const [pickedRef, setPickedRef] = useState<{ groupId: string; checkout: boolean; ref: string }>();
  const gitRef =
    refsAvailable && pickedRef?.groupId === group.id && pickedRef.checkout === checkout
      ? pickedRef.ref
      : undefined;

  // Focus the chosen account when the chooser opens, so a screen reader announces it.
  const chooser = useRef<HTMLFieldSetElement>(null);
  // The heading's repository name opens the same menu as the picker under the box.
  const repoMenu = useId();
  useEffect(() => {
    if (choices) chooser.current?.querySelector<HTMLInputElement>("input:checked")?.focus();
  }, [choices]);

  // Starts `attempt`. Resolves to an error message, or "" when the account chooser opened.
  // `notice` goes to the thread once it starts.
  const attemptStart = async (
    attempt: Attempt,
    askForAccount = true,
    notice?: string,
  ): Promise<string | undefined> => {
    const error = await start(
      attempt.runId,
      attempt.groupId,
      attempt.prompt,
      attempt.images,
      attempt.options,
      attempt.checkout,
      attempt.gitRef,
      attempt.name,
      attempt.threads,
    );
    failed.current = error ? attempt : undefined;
    if (!error) {
      onStarted(attempt.runId, notice, attempt.background);
      return undefined;
    }
    // No default, or one naming a removed key account: both need an account picked. Asks once
    // per Send, so a default that doesn't take can't loop.
    const kind = error.data?.kind;
    if ((kind !== "noDefaultAccount" && kind !== "accountNotFound") || !askForAccount)
      return describeError(error);
    const options = await accountOptions(hostId);
    if (typeof options === "string") return options;
    if (options.length === 0) return noAccounts;
    if (options.length === 1)
      return runOn(
        attempt,
        options[0]!,
        `Using ${options[0]!.label} for new threads on this host.`,
      );
    setPicked(0);
    setChooseError(undefined);
    setChoices(options);
    return "";
  };

  // Makes `option` the worker default, then retries `attempt` with its run id.
  const runOn = async (
    attempt: Attempt,
    option: AccountOption,
    notice?: string,
  ): Promise<string | undefined> => {
    const set = await window.parallax.request(hostId, "accounts/defaults/set", {
      role: "worker",
      account: option.account,
    });
    if ("error" in set) return describeError(set.error);
    return attemptStart(attempt, false, notice);
  };

  const send = async (
    prompt: string,
    options: RunOptions,
    images: PromptImage[],
    threads: string[],
    background: boolean,
  ) => {
    setChoices(undefined);
    const last = failed.current;
    // plxd refuses a run id reused with other options, so changing one starts afresh. It doesn't
    // compare images or threads, so this does: a failed send puts back the very same ones.
    const same =
      last &&
      last.groupId === group.id &&
      last.prompt === prompt &&
      last.images.length === images.length &&
      last.images.every((image, i) => image === images[i]) &&
      last.threads.join() === threads.join() &&
      JSON.stringify(last.options) === JSON.stringify(options) &&
      last.checkout === checkout &&
      last.gitRef === gitRef
        ? last
        : undefined;
    // A background start leaves the box empty for the next thread.
    if (!background) setStarting({ prompt, images, threads });
    // A retry keeps its name, so the same start is the same request. Images alone name nothing.
    const name = same?.name ?? (prompt.trim() ? await window.parallax.nameThread(prompt) : {});
    const error = await attemptStart({
      runId: same?.runId ?? uuidv7(),
      groupId: group.id,
      prompt,
      images,
      threads,
      options,
      checkout,
      gitRef,
      name,
      background,
    });
    // On success the app opens the thread instead.
    if (error !== undefined) setStarting(undefined);
    return error;
  };

  const continueWith = async (option: AccountOption) => {
    setChoosing(true);
    setChooseError(undefined);
    // Continue is a click on New Thread, so the thread opens, even after a Cmd/Ctrl+Enter.
    const error = await runOn({ ...failed.current!, background: false }, option);
    setChoosing(false);
    if (error) setChooseError(error);
  };

  const pickRepository = async () => {
    setRepoError(undefined);
    const path = await window.parallax.pickFolder();
    if (!path) return;
    const repo = await addRepo(path);
    if (typeof repo === "string") setRepoError(repo);
    else onGroupChange(repo.id);
  };

  // While starting, laid out as AgentChat is, so opening the thread doesn't move anything. The
  // Composer keeps its place in the tree either way, so a failed start still puts the text back.
  // Before it starts, it's anchored by its bottom about where centering it a little above the
  // middle would, so the Composer grows upward until the heading reaches the top.
  return (
    <div
      className={
        starting === undefined
          ? "flex flex-1 flex-col items-center justify-end-safe px-6 pb-[calc(56vh-9.375rem)]"
          : "flex min-h-0 flex-1 flex-col"
      }
    >
      {starting !== undefined && (
        <ThreadLinksContext value={threadLinks}>
          <TranscriptView
            rows={[
              {
                kind: "pending",
                key: "pending:prompt",
                text: starting.prompt,
                images: starting.images,
                threads: starting.threads,
              },
            ]}
            sent={new Map()}
            live={false}
          />
        </ThreadLinksContext>
      )}
      <div
        className={
          starting === undefined ? "w-full max-w-3xl" : "mx-auto w-full max-w-3xl px-6 pb-5"
        }
      >
        {starting === undefined && (
          <>
            <h1
              className={`${group.id === noRepo ? "mb-7" : "mb-2"} text-center text-[24px] font-medium tracking-tight`}
            >
              {group.id === noRepo ? "What should we work on " : "What should we build in "}
              <button
                type="button"
                popoverTarget={repoMenu}
                aria-haspopup="menu"
                className="rounded-md underline decoration-muted-foreground decoration-dotted decoration-2 underline-offset-[6px] hover:decoration-foreground hover:decoration-solid"
              >
                {group.id === noRepo ? "without a repo" : group.name}
              </button>
              ?
            </h1>
            {group.id !== noRepo && (
              <div className="relative z-20 mb-9 text-center">
                {/* Its shortcut, Mod+Shift+N unless rebound, shows under it on hover or focus. */}
                <button
                  type="button"
                  onClick={() => {
                    setRepoError(undefined);
                    setChoices(undefined);
                    onGroupChange(noRepo);
                  }}
                  aria-keyshortcuts={noRepoBinding && ariaKeyshortcut(noRepoBinding)}
                  className="group relative rounded-md text-[15px] text-muted-foreground underline underline-offset-4 hover:text-foreground focus-visible:text-foreground"
                >
                  or start without a repo
                  {noRepoKeys && (
                    <kbd
                      aria-hidden
                      className="pointer-events-none invisible absolute top-full left-1/2 mt-1 -translate-x-1/2 rounded-md border border-border bg-surface px-2 py-1 font-sans text-[12px] text-muted-foreground group-hover:visible group-focus-visible:visible"
                    >
                      {noRepoKeys}
                    </kbd>
                  )}
                </button>
              </div>
            )}
            {/* Menu only: the repository name in the heading opens it. */}
            <Picker
              id={repoMenu}
              button={false}
              label="Repository"
              value={group.id}
              onChange={(value) => {
                if (value === addRepository) return void pickRepository();
                setRepoError(undefined);
                setChoices(undefined);
                onGroupChange(value);
              }}
              options={[
                ...groups.map((g) => ({
                  value: g.id,
                  label: g.name,
                  icon: g.id === noRepo ? <House /> : <Folder />,
                })),
                ...(local
                  ? [
                      {
                        value: addRepository,
                        label: "Add repository…",
                        icon: <Plus />,
                        divider: true,
                      },
                    ]
                  : []),
              ]}
            />
          </>
        )}
        <Composer
          newThread
          onSend={(prompt, options, images, threads) =>
            send(prompt, options, images, threads, false)
          }
          onSendInBackground={(prompt, options, images, threads) =>
            send(prompt, options, images, threads, true)
          }
          attach={attachThreads(connection, threadLinks)}
          // Hidden while starting, as the opened thread's composer has none.
          backend={runOptions && starting === undefined ? backend : undefined}
          contextAndFast={
            connection?.status === "connected" && "contextAndFast" in connection.capabilities
          }
          disabledReason={starting === undefined ? disabledReason : "Starting thread…"}
          imageCaps={imageCaps(connection)}
          menus={
            connection?.status === "connected" && "composerMenus" in connection.capabilities
              ? { hostId, repo: group.id === noRepo ? undefined : group.id }
              : undefined
          }
          // A new thread asks only through a plxd that sends its requests (RYA-196).
          manualDenied={
            connection?.status === "connected" && !("approvals" in connection.capabilities)
              ? "host"
              : undefined
          }
          tab={
            starting !== undefined ? (
              <span className={tabItem}>
                <LoaderCircle aria-hidden className="animate-spin" />
                Starting…
              </span>
            ) : (
              <>
                <RunTargetMenu
                  hosts={hosts}
                  hostId={hostId}
                  workspace={checkout ? "checkout" : "worktree"}
                  onWorkspaceChange={setWorkspace}
                  checkoutUnavailable={checkoutUnavailable}
                />
                {refsAvailable && (
                  <RefMenu
                    hostId={hostId}
                    repo={group.id}
                    checkout={checkout}
                    value={gitRef}
                    onChange={(ref) => setPickedRef({ groupId: group.id, checkout, ref })}
                  />
                )}
              </>
            )
          }
          footer={
            <div>
              {repoError && (
                <p role="alert" className="px-2 pt-1.5 text-[12.5px] text-danger">
                  {repoError}
                </p>
              )}
              {choices && (
                <fieldset
                  ref={chooser}
                  className="mt-3 w-full rounded-xl border border-border px-4 py-3"
                >
                  <legend className="float-left mb-2.5 w-full">
                    <span className="block text-[13px] font-medium">
                      Choose an account to run this thread
                    </span>
                    <span className="block text-[12.5px] text-muted-foreground">
                      New threads on this host use it from now on.
                    </span>
                  </legend>
                  <div className="clear-left flex flex-wrap gap-1">
                    {choices.map((choice, i) => (
                      <label
                        key={i}
                        className="flex items-center rounded-md border border-border px-2.5 py-1 text-[12.5px] text-muted-foreground hover:text-foreground has-checked:bg-selected has-checked:text-foreground has-focus-visible:outline-2 has-focus-visible:outline-ring"
                      >
                        <input
                          type="radio"
                          name="account"
                          checked={picked === i}
                          onChange={() => setPicked(i)}
                          className="sr-only"
                        />
                        {choice.label}
                      </label>
                    ))}
                  </div>
                  {chooseError && (
                    <p role="alert" className="pt-2 text-[12.5px] text-danger">
                      {chooseError}
                    </p>
                  )}
                  <button
                    type="button"
                    disabled={choosing}
                    onClick={() => void continueWith(choices[picked]!)}
                    className="mt-3 rounded-md bg-primary px-3 py-1.5 text-[13px] font-medium text-primary-foreground enabled:hover:opacity-90 disabled:opacity-50"
                  >
                    Continue
                  </button>
                </fieldset>
              )}
            </div>
          }
        />
      </div>
    </div>
  );
}
