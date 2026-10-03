import {
  ArrowUp,
  File,
  FilePen,
  Hand,
  ListChecks,
  LoaderCircle,
  Paperclip,
  ShieldOff,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import Bold from "@tiptap/extension-bold";
import Italic from "@tiptap/extension-italic";
import { Fragment, Slice, type Node as ProseMirrorNode } from "@tiptap/pm/model";
import { EditorContent, markInputRule, useEditor, type Editor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { defaultMarkdownSerializer, MarkdownSerializer } from "prosemirror-markdown";
import {
  Fragment as ReactFragment,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from "react";

import type {
  AgentCommand,
  AgentEffort,
  AgentPermission,
  AgentRun,
  PromptImage,
} from "../protocol/generated/protocol";
import { lastPrompt } from "./attention";
import { EffortMenu } from "./EffortMenu";
import { imageUrl, readImage, type ImageCaps } from "./images";
import { ModelMenu } from "./ModelMenu";
import {
  backendOf,
  backends,
  models,
  useDisabledClis,
  type Model,
  type Provider,
  type RunOptions,
} from "./models";
import { lookOf, ThreadChip, type AttachThreads } from "./threadContext";
import { draggedThread, threadDragType } from "./threadDrag";
import { menuItem, Picker, type PickerOption } from "./ui";

// Claude Code's permission modes, under its own names (0027). A thread is full Claude Code in
// every mode (0034), and a project's worker keeps its sandbox in every mode but Bypass (0013).
// What would prompt comes to the chat as approval cards (RYA-196), unless the run can't send them
// (`manualDenied`).
const accessOptions: Record<AgentPermission, PickerOption> = {
  auto: {
    value: "auto",
    label: "Auto",
    icon: <Sparkles />,
    description: "A classifier approves or blocks each action instead of asking you.",
  },
  manual: {
    value: "manual",
    label: "Manual",
    icon: <Hand />,
    description: "Asks you before edits and commands.",
  },
  edit: {
    value: "edit",
    label: "Accept Edits",
    icon: <FilePen />,
    description: "Accepts file edits without asking.",
  },
  plan: {
    value: "plan",
    label: "Plan",
    icon: <ListChecks />,
    description: "Explores and writes a plan without editing files.",
  },
  bypass: {
    value: "bypass",
    label: "Bypass Permissions",
    icon: <ShieldOff />,
    description: "Skips every permission check.",
  },
};

const divider = <span aria-hidden className="mx-1 h-5 w-px bg-border" />;

// The box's editor: typed Markdown (`- `, `1. `, ```` ``` ````, `> `, `#`, `**bold**`, `*italic*`,
// `` `code` ``) formats as you type. Nothing else rewrites what's typed: bold and italic come from
// `**` and `*` only, with no space just inside them (as in CommonMark), so `__init__`, `_private_`,
// and `a * b * c` stay as they are, and there's no strikethrough or `---` rule. Links and
// underline have no place in a prompt, and a trailing empty line after a list or code block would
// only add height.
const extensions = [
  StarterKit.configure({
    bold: false,
    italic: false,
    strike: false,
    horizontalRule: false,
    link: false,
    underline: false,
    trailingNode: false,
  }),
  Bold.extend({
    addInputRules() {
      return [
        markInputRule({ find: /(?:^|\s)(\*\*([^*\s](?:[^*]*[^*\s])?)\*\*)$/, type: this.type }),
      ];
    },
  }),
  Italic.extend({
    addInputRules() {
      return [markInputRule({ find: /(?:^|\s)(\*([^*\s](?:[^*]*[^*\s])?)\*)$/, type: this.type })];
    },
  }),
];

// What's sent is the box as Markdown. Text goes out as typed, unescaped, since the agent reads it
// raw: `foo_bar` and `<div>` stay as they are.
const { nodes, marks } = defaultMarkdownSerializer;
const markdown = new MarkdownSerializer(
  {
    // Paragraphs, headings, and quotes share the defaults' names.
    ...nodes,
    listItem: nodes["list_item"]!,
    bulletList: (state, node) => state.renderList(node, "  ", () => "- "),
    orderedList: (state, node) => {
      // Nested lines indent as far as the widest number reaches.
      const start = node.attrs["start"] as number;
      const width = `${start + node.childCount - 1}. `.length;
      state.renderList(node, " ".repeat(width), (i) => `${start + i}. `.padEnd(width));
    },
    codeBlock: (state, node) => {
      // A fence longer than any run of backticks in the code.
      const runs = node.textContent.match(/`{3,}/g) ?? [];
      const fence = "`".repeat(Math.max(2, ...runs.map((r) => r.length)) + 1);
      state.write(`${fence}${(node.attrs["language"] as string | null) ?? ""}\n`);
      state.text(node.textContent, false);
      state.ensureNewLine();
      state.write(fence);
      state.closeBlock(node);
    },
    hardBreak: (state) => state.write("\n"),
    text: (state, node) => state.text(node.text!, false),
  },
  {
    // So does inline code.
    ...marks,
    bold: marks["strong"]!,
    italic: marks["em"]!,
  },
);

// Lines typed with Shift+Enter are paragraphs, which Markdown would send a blank line apart. They
// go out a line apart, as typed: each run of them is joined into one, with line breaks.
function asLines(node: ProseMirrorNode): ProseMirrorNode {
  if (node.isTextblock) return node;
  const children: ProseMirrorNode[] = [];
  node.forEach((child) => {
    const last = children.at(-1);
    if (last?.type.name === "paragraph" && child.type.name === "paragraph") {
      const lineBreak = child.type.schema.nodes["hardBreak"]!.create();
      children[children.length - 1] = last.copy(
        last.content.addToEnd(lineBreak).append(child.content),
      );
    } else children.push(asLines(child));
  });
  return node.copy(Fragment.from(children));
}

const toMarkdown = (node: ProseMirrorNode) =>
  markdown.serialize(asLines(node), { tightLists: true });

// Whether copied HTML has text of its own, not just an image. A parsed document is inert: nothing
// in it runs or loads.
const hasText = (html: string) =>
  !!new DOMParser().parseFromString(html, "text/html").body.textContent?.trim();

/** Manual's description where its requests are denied, by why (`ComposerProps.manualDenied`). */
const manualDenials = {
  host: "Asks before edits and commands. This host's plxd can't show those requests, so they're denied.",
  run: "Asks before edits and commands. This chat started before Parallax could show those requests, so they're denied.",
};

/** What a `/` or `@` at the start of a word, up to the cursor, is typing: its kind, what follows
 * it, and where it is, for the menus. */
interface Trigger {
  kind: "/" | "@";
  query: string;
  from: number;
  to: number;
}

function triggerAt(editor: Editor): Trigger | undefined {
  const { $from, empty } = editor.state.selection;
  if (!empty || $from.parent.type.spec.code) return undefined;
  const before = $from.parent.textBetween(0, $from.parentOffset, undefined, "\ufffc");
  const match = /(?:^|\s)([/@])(\S*)$/.exec(before);
  if (!match) return undefined;
  const query = match[2]!;
  return { kind: match[1] as "/" | "@", query, from: $from.pos - query.length - 1, to: $from.pos };
}

/** `items` whose name starts with `query`, then those that only contain it, each in order. */
function byName<T>(items: T[], query: string, name: (item: T) => string): T[] {
  const q = query.toLowerCase();
  const starts = (item: T) => name(item).toLowerCase().startsWith(q);
  return [
    ...items.filter(starts),
    ...items.filter((item) => !starts(item) && name(item).toLowerCase().includes(q)),
  ];
}

/** The most files the `@` menu shows. */
const maxFiles = 50;

/** The most threads the `@` menu shows. */
const maxThreadRows = 5;

/**
 * `paths` matching `query`: those whose file name starts with it, then those that contain it,
 * then those that have its letters in order, at most `maxFiles`.
 */
function matchPaths(paths: readonly string[], query: string): string[] {
  const q = query.toLowerCase();
  const rank = (path: string) => {
    const p = path.toLowerCase();
    if (p.slice(p.lastIndexOf("/") + 1).startsWith(q)) return 0;
    if (p.includes(q)) return 1;
    let i = 0;
    for (const c of p) if (c === q[i]) i++;
    return i === q.length ? 2 : 3;
  };
  return paths
    .map((path) => ({ path, rank: rank(path) }))
    .filter((p) => p.rank < 3)
    .sort((a, b) => a.rank - b.rank)
    .slice(0, maxFiles)
    .map((p) => p.path);
}

/** The commands the composer's own controls take, which no CLI gets (PLX-359). */
const ownCommands = new Set(["model", "effort", "fast", "permissions"]);

// Each CLI's commands by host, backend, and folder, kept while the app runs, so reopening a thread
// doesn't start its CLI again. A failed fetch isn't kept, so the next `/` asks again.
const commandLists = new Map<string, Promise<AgentCommand[]>>();

/** The commands `agent/commands` lists for a thread on `backend`, once per host and folder. */
function listCommands(
  hostId: string,
  backend: string,
  repo: string | undefined,
  runId: string | undefined,
): Promise<AgentCommand[]> {
  const key = `${hostId}/${backend}/${runId ?? repo ?? ""}`;
  let list = commandLists.get(key);
  if (!list) {
    list = window.parallax.request(hostId, "agent/commands", { backend, repo, runId }).then((r) => {
      if ("result" in r) return r.result.commands;
      commandLists.delete(key);
      return [];
    });
    commandLists.set(key, list);
  }
  return list;
}

/** A row of the `/` or `@` menu. `group` heads the `@` menu's threads and files apart. */
interface MenuEntry {
  key: string;
  label: string;
  description?: string;
  hint?: string;
  icon?: ReactNode;
  /** Muted text at the row's end, such as how long ago a thread was prompted. */
  meta?: string;
  group?: "Threads" | "Files";
  pick: () => void;
}

/** A plain item in the composer's tab, sized like the pickers that can sit beside it. */
export const tabItem =
  "flex min-w-0 items-center gap-1.5 px-2 py-1 text-[13.5px] text-muted-foreground [&_svg]:size-4 [&_svg]:shrink-0";

/** A prompt for Stop to put back: its text and attached threads, then its images once they load. */
export interface Unanswered {
  text: string;
  images: () => Promise<PromptImage[]>;
  threads?: readonly string[];
}

export interface ComposerProps {
  /** Whether it starts a new thread, which only changes its hint. */
  newThread?: boolean;
  /**
   * Sends the text, images, and attached threads' run ids, with the chosen run options (empty
   * without `backend`). Resolves to an error message, which puts them back; `""` puts them back
   * with no message. Absent: Send stays off.
   */
  onSend?: (
    text: string,
    options: RunOptions,
    images: PromptImage[],
    threads: string[],
  ) => Promise<string | undefined>;
  /** Sends as `onSend` does, for Cmd/Ctrl+Enter anywhere in the box: a new thread's background start. */
  onSendInBackground?: ComposerProps["onSend"];
  /**
   * While set, an empty box shows Stop instead of Send. Resolves to an error message.
   * Stop stays pending until the caller drops `onStop`, when the run stops. Esc in the box stops
   * too.
   */
  onStop?: () => Promise<string | undefined>;
  /** The run's latest prompt while nothing answers it yet, which a Stop that works puts back. */
  unanswered?: Unanswered;
  /** Why sending is off right now, shown in place of the box's hint. */
  disabledReason?: string;
  /** The tab tucked under the box: where the thread runs, or an open run's status. */
  tab?: ReactNode;
  /** What goes under the tab, such as a new thread's account chooser. */
  footer?: ReactNode;
  /**
   * The backend the thread runs on: shows the model, effort, and access choices it can honor. A
   * new thread passes it only when plxd takes run options. Absent (or unknown): no choices, and
   * none are sent. A new thread's model of another provider starts it on that provider's
   * subscription.
   */
  backend?: string;
  /**
   * An open run's model, effort, access, context window, and fast mode (an unset one is the CLI's
   * default). They start from the run's, and only one that differs from it is sent. Another
   * provider's model moves the run to that provider's subscription: all of them go, with the
   * account.
   */
  started?: Pick<AgentRun, "model" | "effort" | "permission" | "contextWindow" | "fast">;
  /** Whether the host's plxd takes a context window and fast mode (`contextAndFast`). */
  contextAndFast?: boolean;
  /** Providers the thread can't run on, by why, whose models it doesn't offer: those an open run
   * can't move to, or a new thread can't start on. */
  unavailable?: Partial<Record<Provider, string>>;
  /** Why the model, effort, and access can't change right now, which turns them off. */
  optionsDisabled?: string;
  /** The host's image caps (`promptImages`). Absent: adding an image just says it can't take them. */
  imageCaps?: ImageCaps;
  /**
   * Why Manual's requests are denied instead of coming to the chat (0031): the host's plxd lacks
   * `approvals`, or the open run started without them. Absent: they come as approval cards.
   */
  manualDenied?: keyof typeof manualDenials;
  /**
   * Text to add at the end of the box, which takes focus, such as a pull request's URL. Each new
   * value is added once.
   */
  insert?: string;
  /** The thread's earlier prompts, oldest first, which Up and Down recall into an empty box. */
  history?: readonly string[];
  /**
   * Where the `/` and `@` menus' lists come from, on a plxd with `composerMenus`: the host, and
   * the thread's run or repo entry, whose folder they're read in. The commands are the CLI's that
   * the message goes to, fetched on the first `/` and kept while the app runs; the files need a run
   * or repo, and are fetched again on each new `@`. Absent: no menus.
   */
  menus?: { hostId: string; repo?: string; runId?: string };
  /**
   * The host's threads, on a plxd with `threadContext` (0047): `@` lists them above the files, and
   * a sidebar row dropped on the box attaches one, as a chip beside the images. Absent: neither.
   */
  attach?: AttachThreads;
}

/**
 * The prompt box, the same on every screen. It formats Markdown as you type and sends it as
 * Markdown text. Enter sends and Shift+Enter starts a new line (a new item, in a list); in a code
 * block Enter adds a line and Cmd/Ctrl+Enter sends. With `onSendInBackground`, Cmd/Ctrl+Enter sends
 * through it, anywhere in the box. It grows with its text up to 40% of the window.
 * Pasted, dropped, and picked images sit above the text as thumbnails, and go beside it, never in
 * it (RYA-193).
 * In an empty box, Up and Down step through `history`, until the recalled prompt is edited.
 * With `menus`, `/` at the start of a word opens a menu of the composer's own commands and the
 * CLI's commands and skills, and `@` one of the thread's files, filtered as you type. Up and Down
 * move through it, Enter or Tab picks, and Esc closes it.
 * With `attach`, `@` also lists the host's threads above the files: its newest, or those
 * `thread/search` finds for what's typed. Picking one, or dropping a sidebar row on the box,
 * attaches it as a chip beside the images (PLX-378).
 */
export function Composer({
  newThread,
  onSend,
  onSendInBackground,
  onStop,
  unanswered,
  disabledReason,
  tab,
  footer,
  backend,
  started,
  contextAndFast,
  unavailable,
  optionsDisabled,
  imageCaps,
  manualDenied,
  insert,
  history = [],
  menus,
  attach,
}: ComposerProps) {
  // The box as Markdown, kept on every edit.
  const [text, setText] = useState("");
  const [error, setError] = useState<string>();
  const [stopping, setStopping] = useState(false);
  // Files that aren't images, shown as chips; plxd doesn't take them yet.
  const [files, setFiles] = useState<File[]>([]);
  const [images, setImages] = useState<PromptImage[]>([]);
  // Attached threads' run ids, shown as chips beside the images.
  const [threads, setThreads] = useState<string[]>([]);
  // Why an image or thread wasn't added, shown by the thumbnails.
  const [attachError, setAttachError] = useState<string>();
  // Whether a sidebar row is dragged over the box, which outlines it.
  const [threadOver, setThreadOver] = useState(false);
  const filePicker = useRef<HTMLInputElement>(null);
  // Which of `history` the box holds, unedited.
  const recalled = useRef<number>(undefined);
  const [pickedModel, setModel] = useState<Model>();
  const [pickedEffort, setEffort] = useState<AgentEffort>();
  const [pickedPermission, setPermission] = useState<AgentPermission>();
  const [pickedContext, setContext] = useState<number>();
  const [pickedFast, setFast] = useState<boolean>();
  // What `backend` can honor: another backend's pick falls back to its first model and `edit`.
  const run = backend === undefined ? undefined : backends[backend];
  const runModels = models.filter((m) => m.provider === run?.provider);
  // Providers turned off in Settings, but an open run's own, which it keeps.
  const off = useDisabledClis();
  const blocked = { ...unavailable };
  for (const cli of off) {
    const p = backends[cli]?.provider;
    if (p && !(started && p === run?.provider))
      blocked[p] ??= `${p} is turned off in Settings > Providers.`;
  }
  // Any provider whose models aren't unavailable: an open run moves to it, a new thread starts there.
  const choices = models.filter((m) => !blocked[m.provider]);
  // An open run's model, which may be one this list doesn't know, or the CLI's default.
  const startedModel =
    started &&
    run &&
    (runModels.find((m) => m.id === started.model) ?? {
      id: started.model ?? "",
      name: started.model ?? "Default model",
      provider: run.provider,
      contexts: [],
    });
  const model =
    choices.find((m) => m === pickedModel) ??
    startedModel ??
    runModels.find((m) => choices.includes(m)) ??
    choices[0];
  // Where the message goes: the run's backend, or the one that runs the picked model.
  const target =
    run && model && model.provider !== run.provider ? backendOf(model.provider) : backend;
  const targetBackend = target === undefined ? undefined : backends[target];
  const permissions = targetBackend?.permissions ?? [];
  // A backend that maps no efforts (Cursor) gets none, and shows no effort menu.
  const efforts = targetBackend?.efforts !== false;
  const startedEffort = started?.effort ?? "high";
  const startedPermission = started?.permission ?? "edit";
  const effort = pickedEffort ?? startedEffort;
  const wanted = pickedPermission ?? startedPermission;
  const permission = permissions.includes(wanted) ? wanted : "edit";
  // The model's context windows and fast mode, on a plxd that takes them. One the model doesn't
  // offer falls back to its default, and fast mode to off.
  const contexts = (contextAndFast && model?.contexts) || [];
  const startedContext = started?.contextWindow ?? startedModel?.contexts[0];
  const wantedContext = pickedContext ?? startedContext;
  const context =
    wantedContext !== undefined && contexts.includes(wantedContext) ? wantedContext : contexts[0];
  const hasFast = !!contextAndFast && !!model?.fast;
  const startedFast = started?.fast ?? false;
  const fast = hasFast && (pickedFast ?? startedFast);
  const speed = {
    ...(context !== undefined && { contextWindow: context }),
    ...(hasFast && { fast }),
  };
  const account = { kind: "subscription", backend: target! } as const;
  let options: RunOptions = {};
  if (run && started && model && target !== backend)
    options = { model: model.id, ...(efforts && { effort }), permission, ...speed, account };
  else if (run && started)
    options = {
      ...(model && model !== startedModel && { model: model.id }),
      ...(efforts && effort !== startedEffort && { effort }),
      ...(permission !== startedPermission && { permission }),
      ...(context !== undefined && context !== startedContext && { contextWindow: context }),
      ...(hasFast && fast !== startedFast && { fast }),
    };
  else if (run)
    options = {
      ...(model && { model: model.id }),
      ...(efforts && { effort }),
      permission,
      ...speed,
      ...(target !== backend && { account }),
    };
  // The `/` and `@` menus: what's typed, the lists, and the highlighted row. Esc closes the menu
  // until its `/` or `@` goes.
  const [trigger, setTrigger] = useState<Trigger>();
  const [closedAt, setClosedAt] = useState<number>();
  const [active, setActive] = useState(0);
  // Each list with what it was fetched for, so a list for another backend or folder isn't shown.
  const [commands, setCommands] = useState<{ key: string; list: AgentCommand[] }>();
  const [paths, setPaths] = useState<{ key: string; list: string[] }>();
  const controls = useRef<HTMLFieldSetElement>(null);
  const menuId = useId();
  const { hostId, repo, runId } = menus ?? {};
  const folder = runId ?? repo ?? "";
  const commandsKey =
    hostId === undefined || target === undefined ? undefined : `${hostId}/${target}/${folder}`;
  const pathsKey = hostId === undefined || !folder ? undefined : `${hostId}/${folder}`;
  // Fetched on the first `/` or `@`, not before: listing commands starts the CLI.
  const wantsCommands = trigger?.kind === "/" && commandsKey !== undefined;
  const wantsPaths = trigger?.kind === "@" && pathsKey !== undefined;
  useEffect(() => {
    if (!wantsCommands || hostId === undefined || target === undefined) return;
    let live = true;
    void listCommands(hostId, target, repo, runId).then(
      (list) => live && setCommands({ key: commandsKey, list }),
    );
    return () => {
      live = false;
    };
  }, [wantsCommands, commandsKey, hostId, target, repo, runId]);
  // Each new `@` lists them again, so files the agent made show up; the last list shows meanwhile.
  const pathsAt = wantsPaths ? trigger.from : undefined;
  useEffect(() => {
    if (pathsAt === undefined || hostId === undefined || pathsKey === undefined) return;
    let live = true;
    void window.parallax
      .request(hostId, "repo/files", { repo, runId })
      .then((r) => live && setPaths({ key: pathsKey, list: "result" in r ? r.result.files : [] }));
    return () => {
      live = false;
    };
  }, [pathsAt, pathsKey, hostId, repo, runId]);
  const loadedCommands = commands && commands.key === commandsKey ? commands.list : undefined;
  const loadedPaths = paths && paths.key === pathsKey ? paths.list : undefined;
  // The threads `thread/search` found for what's typed after `@`; the last list shows meanwhile.
  const [found, setFound] = useState<string[]>([]);
  const threadHost = attach?.hostId;
  const search = trigger?.kind === "@" ? trigger.query.trim() : undefined;
  useEffect(() => {
    if (threadHost === undefined || !search) return;
    let live = true;
    void window.parallax
      .request(threadHost, "thread/search", { query: search })
      .then((r) => live && setFound("result" in r ? r.result.threads.map((t) => t.id) : []));
    return () => {
      live = false;
    };
  }, [threadHost, search]);
  // Adds threads as chips, once each, at most the host's cap, but never the open thread itself.
  const addThreads = (ids: readonly string[]) => {
    const all = [...new Set([...threads, ...ids])].filter((id) => id !== attach?.self);
    const max = attach?.max ?? 0;
    setAttachError(
      all.length > max ? `A message takes at most ${max} attached threads.` : undefined,
    );
    setThreads(all.slice(0, max));
  };

  // Puts `text` where the trigger is, as typed text, which Markdown leaves alone.
  const replaceTrigger = (at: Trigger, text: string) =>
    editor
      .chain()
      .focus()
      .command(({ tr }) => !!tr.insertText(text, at.from, at.to))
      .run();
  // The composer's own controls, as commands where they're shown and can change.
  const openControl = (label: string) =>
    controls.current?.querySelector<HTMLButtonElement>(`[aria-label^="${label}"]`)?.click();
  const own =
    !run || optionsDisabled
      ? []
      : [
          model && {
            name: "model",
            description: "Pick the model",
            act: () => openControl("Model:"),
          },
          efforts && {
            name: "effort",
            description: "Set the reasoning effort",
            act: () => openControl("Reasoning effort:"),
          },
          efforts &&
            hasFast && {
              name: "fast",
              description: fast ? "Turn fast mode off" : "Turn fast mode on",
              act: () => setFast(!fast),
            },
          permissions.length > 1 && {
            name: "permissions",
            description: "Choose what it may do without asking",
            act: () => openControl("Access:"),
          },
        ].filter((c) => !!c);
  let entries: MenuEntry[] = [];
  if (trigger?.kind === "/")
    entries = [
      ...byName(own, trigger.query, (c) => c.name).map((c) => ({
        key: `own:${c.name}`,
        label: `/${c.name}`,
        description: c.description,
        pick: () => {
          replaceTrigger(trigger, "");
          c.act();
        },
      })),
      ...byName(
        (loadedCommands ?? []).filter((c) => !ownCommands.has(c.name)),
        trigger.query,
        (c) => c.name,
      ).map((c, i) => ({
        // Two scopes can each have a skill of the same name.
        key: `${i}:${c.text}`,
        label: c.text,
        description: c.description,
        hint: c.argumentHint,
        pick: () => replaceTrigger(trigger, `${c.text} `),
      })),
    ];
  else if (trigger?.kind === "@") {
    // With nothing typed, the host's newest threads, as the sidebar orders them.
    const ids = !attach
      ? []
      : search
        ? found
        : attach.state.threads
            .filter((t) => !t.archived)
            .sort((a, b) => Date.parse(lastPrompt(b)) - Date.parse(lastPrompt(a)))
            .map((t) => t.id);
    const threadRows = ids
      .filter((id) => id !== attach?.self && !threads.includes(id))
      .slice(0, maxThreadRows)
      .map((id): MenuEntry => {
        const look = lookOf(attach?.state, id);
        return {
          key: `thread:${id}`,
          label: look.title,
          icon: <look.Logo aria-hidden />,
          meta: look.age,
          group: "Threads",
          pick: () => {
            replaceTrigger(trigger, "");
            addThreads([id]);
          },
        };
      });
    const fileRows = matchPaths(loadedPaths ?? [], trigger.query).map((path): MenuEntry => ({
      key: path,
      label: path,
      // Headed only beside threads.
      group: threadRows.length > 0 ? "Files" : undefined,
      pick: () => replaceTrigger(trigger, `@${path} `),
    }));
    entries = [...threadRows, ...fileRows];
  }
  // What the menu still waits for, shown as a row of its own.
  const loading =
    (wantsCommands && !loadedCommands && "Loading commands…") ||
    (wantsPaths && !loadedPaths && "Loading files…") ||
    undefined;
  const menuOpen =
    !!menus && !!trigger && trigger.from !== closedAt && (entries.length > 0 || !!loading);
  const highlighted = Math.min(active, entries.length - 1);
  useEffect(() => {
    document.getElementById(`${menuId}-${highlighted}`)?.scrollIntoView?.({ block: "nearest" });
  }, [menuId, highlighted, menuOpen]);

  // The run stopped (or never ran), so a later run's Stop starts fresh.
  if (stopping && !onStop) setStopping(false);
  const empty = text.trim() === "" && images.length === 0;
  const canSend = !!onSend && !disabledReason && !empty;
  const showStop = !!onStop && !disabledReason && empty;

  // Adds pasted, dropped, or picked files: images as thumbnails, anything else as a chip. Each
  // image gets an even share of the total cap, so any number of them up to the most fits it.
  const addFiles = async (added: File[]) => {
    const isImage = (f: File) => f.type.startsWith("image/");
    setFiles((all) => [...all, ...added.filter((f) => !isImage(f))]);
    const picked = added.filter(isImage);
    setAttachError(undefined);
    if (picked.length === 0) return;
    if (!imageCaps) return setAttachError("This host's plxd can't take images.");
    const { maxImages, maxImageBytes, maxTotalBytes } = imageCaps;
    const room = Math.max(0, maxImages - images.length);
    const share = Math.min(maxImageBytes, Math.floor(maxTotalBytes / maxImages));
    const read = await Promise.all(picked.slice(0, room).map((f) => readImage(f, share)));
    const errors = read.filter((r) => typeof r === "string");
    if (picked.length > room) errors.push(`A message takes at most ${maxImages} images.`);
    setAttachError(errors[0]);
    const ok = read.filter((r) => typeof r !== "string");
    setImages((all) => [...all, ...ok].slice(0, maxImages));
  };

  const submit = async (background = false) => {
    if (!canSend) return;
    const sent = editor.getJSON();
    const sentImages = images;
    const sentThreads = threads;
    editor.commands.clearContent();
    setImages([]);
    setThreads([]);
    setError(undefined);
    setAttachError(undefined);
    const failed = await (background ? onSendInBackground! : onSend)(
      text,
      options,
      sentImages,
      sentThreads,
    );
    if (failed === undefined) setFiles([]);
    else if (!editor.isDestroyed) {
      // Put it back ahead of anything typed or added while it was in flight.
      const typed = editor.isEmpty ? [] : (editor.getJSON().content ?? []);
      editor.commands.setContent({ ...sent, content: [...(sent.content ?? []), ...typed] });
      setImages((added) => [...sentImages, ...added].slice(0, imageCaps?.maxImages));
      putBackThreads(sentThreads);
      setError(failed);
    }
  };

  // Threads back ahead of any attached meanwhile.
  const putBackThreads = (back: readonly string[]) =>
    setThreads((added) => [...new Set([...back, ...added])].slice(0, attach?.max));

  const stop = async () => {
    const back = unanswered;
    setStopping(true);
    setError(undefined);
    const failed = await onStop?.();
    if (failed) {
      setStopping(false);
      setError(failed);
    } else if (back && !editor.isDestroyed) {
      // Back ahead of anything typed meanwhile, as plain lines: they send as the same Markdown.
      // ponytail: its formatting shows as typed Markdown, until the box parses Markdown.
      editor.commands.focus("start");
      editor.view.pasteText(editor.isEmpty ? back.text : `${back.text}\n`);
      if (attach && back.threads?.length) putBackThreads(back.threads);
      void back
        .images()
        .then((images) =>
          setImages((added) => [...images, ...added].slice(0, imageCaps?.maxImages)),
        );
    }
  };

  const placeholder =
    disabledReason ??
    (newThread
      ? "Describe a change, paste an error, or drop in a plan"
      : "Reply, add detail, or steer what it does next");
  // Its props are read again on every render, so its handlers see this render's state.
  const editor: Editor = useEditor({
    extensions,
    // Pasted text arrives as typed, never reformatted.
    enablePasteRules: false,
    onUpdate: ({ editor, transaction }) => {
      if (!transaction.getMeta("recall")) recalled.current = undefined;
      setText(toMarkdown(editor.state.doc));
    },
    onSelectionUpdate: ({ editor }) => {
      const at = triggerAt(editor);
      setTrigger(at);
      setActive(0);
      if (!at) setClosedAt(undefined);
    },
    editorProps: {
      // All of them, since these replace Tiptap's own (its role too) once props change.
      attributes: {
        id: "composer-input",
        role: "textbox",
        "aria-label": "Message",
        "aria-multiline": "true",
        "aria-placeholder": placeholder,
        "aria-autocomplete": "list",
        "aria-expanded": String(menuOpen),
        ...(menuOpen &&
          entries.length > 0 && {
            "aria-controls": menuId,
            "aria-activedescendant": `${menuId}-${highlighted}`,
          }),
        // It grows from three rows up to the cap, then scrolls. Its parent is anchored below it,
        // so it grows upward.
        class:
          "composer-input markdown block max-h-[40vh] min-h-[calc(4.875em+1.125rem)] overflow-y-auto px-5 pt-4.5 focus-visible:outline-none",
      },
      handleKeyDown: (_view, event): boolean => {
        const plain = !event.shiftKey && !event.altKey && !event.metaKey && !event.ctrlKey;
        if (menuOpen && plain && !event.isComposing) {
          const step = { ArrowDown: 1, ArrowUp: -1 }[event.key];
          if (event.key === "Escape") setClosedAt(trigger.from);
          // Only a loading row: the keys do what they do without a menu.
          else if (entries.length === 0) return false;
          else if (step) setActive((highlighted + step + entries.length) % entries.length);
          else if (event.key === "Enter" || event.key === "Tab") entries[highlighted]!.pick();
          else return false;
          return true;
        }
        if ((event.key === "ArrowUp" || event.key === "ArrowDown") && plain) {
          const at = recalled.current;
          if (at === undefined && !(event.key === "ArrowUp" && editor.isEmpty && history.length))
            return false;
          // Up stops at the oldest, and Down past the newest empties the box.
          const next = Math.max(0, (at ?? history.length) + (event.key === "ArrowUp" ? -1 : 1));
          recalled.current = next < history.length ? next : undefined;
          // A paragraph a line, as pasted text is, with the cursor at its end.
          const lines = (history[next] ?? "").split("\n").map((line) => ({
            type: "paragraph",
            ...(line && { content: [{ type: "text", text: line }] }),
          }));
          editor
            .chain()
            .setMeta("recall", true)
            .setContent(next < history.length ? { type: "doc", content: lines } : "")
            .focus("end")
            .run();
          return true;
        }
        if (event.key === "Escape" && showStop && !stopping && !event.isComposing) {
          void stop();
          return true;
        }
        if (event.key !== "Enter" || event.isComposing) return false;
        const inCode = editor.isActive("codeBlock");
        const mod = event.metaKey || event.ctrlKey;
        if (inCode ? mod : !event.shiftKey) {
          void submit(!!onSendInBackground && mod);
          return true;
        }
        // Shift+Enter does what Enter does in other editors: a new line, list item, or line of
        // code, or out of an empty list item. A line of just ``` or ```lang starts a code block,
        // as ``` and a space does.
        return (
          event.shiftKey &&
          editor.commands.first(({ commands }) => [
            () => commands.newlineInCode(),
            ({ state }) => {
              const { $from } = state.selection;
              const fence = /^```([a-z]*)$/.exec($from.parent.textContent);
              return (
                !!fence &&
                commands.deleteRange({ from: $from.start(), to: $from.end() }) &&
                commands.setCodeBlock(fence[1] ? { language: fence[1] } : undefined)
              );
            },
            () => commands.splitListItem("listItem"),
            () => commands.liftEmptyBlock(),
            () => commands.splitBlock(),
          ])
        );
      },
      // Paste takes files (a screenshot, a copied image) above the text, so a copied file's name
      // or an image's URL never lands in it. Text copied from an app (Office, Notes, a web page)
      // can carry a picture of itself too: when its HTML has text of its own, it's text. Text is
      // plain only, so nothing brings in its source's styling. Anything else falls through to the
      // editor, which drops it.
      handleDOMEvents: {
        paste: (view, event) => {
          const data = event.clipboardData;
          const text = data?.getData("text/plain");
          const html = data?.getData("text/html");
          let files = [...(data?.files ?? [])];
          if (files.length > 0 && text && html && hasText(html)) files = [];
          if (files.length === 0 && !text) return false;
          event.preventDefault();
          if (files.length > 0) void addFiles(files);
          else view.pasteText(text!);
          return true;
        },
      },
      // Copy and cut give the selection's Markdown as its text, so pasting it back sends the same.
      // Within one line or code block, that's just its text (with any inline Markdown), not the
      // block's markers or fences.
      clipboardTextSerializer: (slice, view) => {
        const { selection, schema } = view.state;
        const { $from, $to } = selection;
        const content =
          $from.sameParent($to) && $from.parent.isTextblock
            ? schema.nodes["paragraph"]!.create(
                null,
                $from.parent.slice($from.parentOffset, $to.parentOffset).content,
              )
            : slice.content;
        return toMarkdown(schema.topNodeType.create(null, content));
      },
      // Pasted lines are lines, as typed ones are: a paragraph each, blank ones kept.
      clipboardTextParser: (text, _context, _plain, view) => {
        const { schema } = view.state;
        const lines = text
          .split(/\r\n?|\n/)
          .map((line) => schema.nodes["paragraph"]!.create(null, line ? schema.text(line) : null));
        return new Slice(Fragment.from(lines), 1, 1);
      },
    },
  });

  useEffect(() => {
    if (!insert) return;
    // A space apart from what's typed.
    const space = editor.isEmpty || /\s$/.test(editor.getText()) ? "" : " ";
    editor
      .chain()
      .focus("end")
      .insertContent(space + insert)
      .run();
  }, [insert, editor]);

  return (
    <div className="w-full">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
        // Files and sidebar threads dropped anywhere on the box are added, before the editor can
        // take them as text.
        onDragOver={(e) => {
          const { types } = e.dataTransfer;
          const thread = !!attach && types.includes(threadDragType);
          if (types.includes("Files") || thread) e.preventDefault();
          if (thread) e.dataTransfer.dropEffect = "copy";
          setThreadOver(thread);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setThreadOver(false);
        }}
        onDropCapture={(e) => {
          setThreadOver(false);
          const thread = attach ? draggedThread(e.dataTransfer) : undefined;
          if (e.dataTransfer.files.length === 0 && !thread) return;
          e.preventDefault();
          e.stopPropagation();
          if (!thread) return void addFiles([...e.dataTransfer.files]);
          // Run ids are the host's own, so another computer's thread can't come along.
          if (thread.hostId !== attach?.hostId)
            setAttachError("A thread on another computer can't be attached here.");
          else addThreads([thread.runId]);
        }}
        className={`relative z-10 rounded-3xl border bg-surface shadow-composer focus-within:border-ring ${threadOver ? "border-ring" : "border-border"}`}
      >
        {menuOpen && (
          <div
            id={menuId}
            role="listbox"
            aria-label={
              trigger.kind === "/"
                ? "Commands"
                : entries[0]?.group === "Threads"
                  ? "Threads and files"
                  : "Files"
            }
            className="absolute inset-x-0 bottom-full mb-2 max-h-72 overflow-y-auto rounded-lg border border-border bg-surface p-1 text-foreground shadow-composer"
          >
            {entries.map((entry, i) => (
              <ReactFragment key={entry.key}>
                {entry.group && entry.group !== entries[i - 1]?.group && (
                  <div
                    role="presentation"
                    className="px-2 pt-1.5 pb-1 text-[11.5px] font-medium text-faint-foreground"
                  >
                    {entry.group}
                  </div>
                )}
                <div
                  id={`${menuId}-${i}`}
                  role="option"
                  aria-selected={i === highlighted}
                  // The box keeps focus, so typing goes on filtering.
                  onMouseDown={(e) => {
                    e.preventDefault();
                    entry.pick();
                  }}
                  onMouseMove={() => setActive(i)}
                  className={`${menuItem} cursor-default ${i === highlighted ? "bg-hover" : ""}`}
                >
                  {entry.icon}
                  <span className={entry.meta ? "min-w-0 truncate" : "shrink-0"}>
                    {entry.label}
                  </span>
                  {entry.hint && (
                    <span className="shrink-0 text-[12px] text-faint-foreground">{entry.hint}</span>
                  )}
                  {entry.description && (
                    <span className="min-w-0 truncate text-[12px] text-faint-foreground">
                      {entry.description}
                    </span>
                  )}
                  {entry.meta && (
                    <span className="ml-auto shrink-0 pl-2 text-[12px] text-faint-foreground">
                      {entry.meta}
                    </span>
                  )}
                </div>
              </ReactFragment>
            ))}
            {loading && (
              <p role="status" className="px-2 py-1.5 text-[12px] text-faint-foreground">
                {loading}
              </p>
            )}
          </div>
        )}
        {(images.length > 0 || threads.length > 0 || attachError) && (
          <div className="flex flex-wrap items-center gap-2 px-4 pt-3.5">
            {threads.map((id) => (
              <ThreadChip
                key={id}
                look={lookOf(attach?.state, id)}
                onRemove={() => setThreads((all) => all.filter((t) => t !== id))}
              />
            ))}
            {images.map((image, i) => (
              <span key={i} className="relative">
                <img
                  src={imageUrl(image)}
                  alt={`Image ${i + 1}`}
                  className="size-14 rounded-xl border border-border object-cover"
                />
                <button
                  type="button"
                  aria-label={`Remove image ${i + 1}`}
                  onClick={() => setImages((all) => all.filter((_, j) => j !== i))}
                  className="absolute -top-1.5 -right-1.5 grid size-5 place-items-center rounded-full border border-border bg-surface text-muted-foreground shadow-sm hover:text-foreground"
                >
                  <X className="size-3" />
                </button>
              </span>
            ))}
            {attachError && (
              <p role="alert" className="text-[12.5px] text-danger">
                {attachError}
              </p>
            )}
          </div>
        )}
        <div className="relative">
          {!text && (
            <p
              aria-hidden
              className="pointer-events-none absolute inset-x-5 top-4.5 truncate text-[15px] leading-relaxed text-faint-foreground"
            >
              {placeholder}
            </p>
          )}
          <EditorContent editor={editor} />
        </div>
        {files.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5 px-4 pt-2">
            {files.map((f, i) => (
              <span
                key={i}
                className="flex items-center gap-1.5 rounded-lg bg-selected py-1 pr-1 pl-2 text-[12.5px]"
              >
                <File aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                <span className="max-w-48 truncate">{f.name}</span>
                <button
                  type="button"
                  aria-label={`Remove ${f.name}`}
                  onClick={() => setFiles((all) => all.filter((_, j) => j !== i))}
                  className="grid size-5 place-items-center rounded text-muted-foreground hover:bg-hover hover:text-foreground"
                >
                  <X className="size-3" />
                </button>
              </span>
            ))}
            <span className="text-[12px] text-faint-foreground">Not sent to the agent yet</span>
          </div>
        )}
        <div className="flex items-center gap-0.5 px-3 pt-1 pb-3">
          {run && (
            <>
              {/* A disabled fieldset turns off every control in it, and its title says why. */}
              <fieldset
                ref={controls}
                disabled={!!optionsDisabled}
                title={optionsDisabled}
                className="flex min-w-0 items-center gap-0.5"
              >
                {model && (
                  <>
                    {/* Every provider, and an open run can't pick those it can't move to. */}
                    <ModelMenu
                      key={backend}
                      models={models}
                      unavailable={blocked}
                      value={model}
                      onChange={setModel}
                    />
                  </>
                )}
                {efforts && (
                  <>
                    {divider}
                    <EffortMenu
                      value={effort}
                      onChange={setEffort}
                      contexts={contexts}
                      context={context}
                      onContext={setContext}
                      fastMode={hasFast ? model!.provider : undefined}
                      fast={fast}
                      onFast={setFast}
                    />
                  </>
                )}
                {/* One permission is no choice, so there's nothing to show. */}
                {permissions.length > 1 && (
                  <>
                    {divider}
                    <Picker
                      label="Access"
                      value={permission}
                      onChange={(value) => setPermission(value as AgentPermission)}
                      options={permissions.map((p) =>
                        p === "manual" && manualDenied
                          ? { ...accessOptions[p], description: manualDenials[manualDenied] }
                          : accessOptions[p],
                      )}
                      panelClassName="w-[25rem]"
                    />
                  </>
                )}
              </fieldset>
            </>
          )}
          <input
            ref={filePicker}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              void addFiles(Array.from(e.target.files ?? []));
              e.target.value = "";
            }}
          />
          <button
            type="button"
            aria-label="Attach files"
            title="Attach files"
            onClick={() => filePicker.current?.click()}
            className="mr-1.5 ml-auto grid size-9 place-items-center rounded-full text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-4.5"
          >
            <Paperclip />
          </button>
          {showStop ? (
            <button
              type="button"
              aria-label={stopping ? "Stopping" : "Stop"}
              disabled={stopping}
              onClick={() => void stop()}
              className="grid size-9 place-items-center rounded-full bg-danger text-background disabled:opacity-50"
            >
              {stopping ? (
                <LoaderCircle className="size-4.5 animate-spin" />
              ) : (
                <Square className="size-3.5 fill-current" />
              )}
            </button>
          ) : (
            <button
              type="submit"
              aria-label="Send"
              disabled={!canSend}
              className="grid size-9 place-items-center rounded-full bg-send text-send-foreground disabled:opacity-25"
            >
              <ArrowUp className="size-4.5" />
            </button>
          )}
        </div>
      </form>
      {tab && (
        // Tucked under the box, so what it shows reads as part of it.
        <div className="mx-5 -mt-4 flex min-w-0 items-center justify-between gap-2 rounded-b-3xl border border-t-0 border-border bg-surface px-3 pt-5 pb-1.5">
          {tab}
        </div>
      )}
      {error && (
        <p role="alert" className="px-2 pt-2 text-[13px] text-danger">
          {error}
        </p>
      )}
      {footer}
    </div>
  );
}
