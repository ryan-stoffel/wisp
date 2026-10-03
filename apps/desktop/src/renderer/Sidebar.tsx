import {
  AlarmClock,
  Archive,
  ArchiveRestore,
  ArrowLeft,
  Bot,
  ChartNoAxesColumn,
  Check,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CirclePause,
  CircleSlash,
  CircleUser,
  Ellipsis,
  FileDiff,
  Folder,
  FolderPlus,
  GitBranch,
  GitMerge,
  HardDrive,
  Keyboard,
  Laptop,
  ListFilter,
  LoaderCircle,
  Palette,
  PanelLeftClose,
  Plus,
  Search,
  Server,
  Settings,
  SquareDashed,
  SquarePen,
  type LucideIcon,
} from "lucide-react";
import {
  useEffect,
  useId,
  useRef,
  useState,
  type ComponentType,
  type ReactNode,
  type SVGProps,
  type ToggleEvent,
} from "react";

import type {
  AgentRun,
  AgentStatus,
  Project,
  ProjectIcon as ProjectIconValue,
  Repo,
  Thread,
} from "../protocol/generated/protocol";
import type { Selection, SettingsSection } from "./App";
import { clock } from "./Approval";
import { AddRepositoryDialog } from "./AddRepositoryDialog";
import {
  attentionOf,
  initials,
  lastPrompt,
  projectAttention,
  snoozeChoices,
  snoozed,
  type Attention,
} from "./attention";
import { AttentionBadge } from "./AttentionMark";
import { ConnectionStatus } from "./ConnectionStatus";
import { Avatar, useProfile } from "./profile";
import { localId, type Host } from "./hosts";
import { IconPicker } from "./IconPicker";
import { imageUrl } from "./images";
import { ClaudeLogo, CursorLogo, OpenAILogo, ParallaxMark } from "./logos";
import { NewProjectDialog } from "./NewProjectDialog";
import { iconColors, iconLook } from "./projectIcons";
import { dragThread } from "./threadDrag";
import { asksOf, type ProjectChange, type ThreadsView } from "./threads";
import { accountLabel, isRunning, statusLabel as runStatusLabel } from "./transcript";
import {
  IconButton,
  menuItem,
  menuPanel,
  moveFocus,
  openOnContextMenu,
  RowBadge,
  rowShortcut,
  TopBar,
  useModHeld,
} from "./ui";
import { UpdateButton } from "./Update";

const row =
  "flex w-full items-center gap-2 rounded-md px-2 py-[5px] text-left text-[13px] hover:bg-hover";
const current = "bg-selected text-foreground";
const sectionHeading =
  "flex h-7 items-center gap-1 px-2 text-[12px] font-medium text-faint-foreground";
const emptyNote = "px-2 py-1 text-[12.5px] text-faint-foreground";

interface SidebarProps {
  open: boolean;
  onClose: () => void;
  onNewThread: () => void;
  children: ReactNode;
}

/**
 * The left column. Its top row holds the macOS traffic lights, then the toggle at the same
 * spot the main pane shows it while this column is hidden, then the app's mark and name, which
 * open a new thread.
 */
export function Sidebar({ open, onClose, onNewThread, children }: SidebarProps) {
  return (
    <nav
      id="sidebar"
      aria-label="Sidebar"
      hidden={!open}
      className="flex w-64 shrink-0 flex-col border-r border-border bg-sidebar"
    >
      <TopBar className="traffic-light-inset">
        <IconButton
          label="Hide sidebar"
          command="sidebar"
          aria-expanded
          aria-controls="sidebar"
          onClick={onClose}
        >
          <PanelLeftClose />
        </IconButton>
        <button
          type="button"
          onClick={onNewThread}
          className="mr-auto flex items-center gap-1.5 rounded-md px-1 py-0.5 font-brand text-[14px] font-semibold tracking-tight text-foreground hover:bg-hover"
        >
          <ParallaxMark className="size-5" />
          Parallax
        </button>
      </TopBar>
      {children}
    </nav>
  );
}

/** A host and its threads and Projects, as the sidebar merges them. */
export interface HostThreads {
  host: Host;
  view: ThreadsView;
}

interface ThreadListProps {
  /** Every host's list, this computer first. */
  hosts: HostThreads[];
  /** The open host: where a new Project goes by default, and whose connection the footer shows. */
  host: Host;
  selection: Selection;
  /** Opens something on `hostId`, which becomes the open host. */
  onSelect: (hostId: string, selection: Selection) => void;
  /** Opens a Project, on another host by opening that host first. */
  onOpenProject: (hostId: string, projectId: string) => void;
  onOpenSettings: (section: SettingsSection) => void;
  /** Deletes a thread. Resolves to an error message, or undefined. */
  onDelete: (hostId: string, thread: Thread) => Promise<string | undefined>;
  onNewThread: () => void;
}

/**
 * A Project's icon, in the sidebar, the breadcrumb, its chat, and Create Project: its uploaded
 * image as a rounded square (0038), else its glyph in its color, or `FolderKanban` in the accent
 * for none, or for a name or color this app doesn't know.
 */
export function ProjectIcon({
  icon,
  className = "",
}: {
  icon?: ProjectIconValue;
  className?: string;
}) {
  // An <svg> like the glyph's, so every place's glyph size (`[&_svg]:size-*`) fits the image too.
  if (icon?.image)
    return (
      <svg
        aria-hidden
        data-icon-image
        viewBox="0 0 24 24"
        width={24}
        height={24}
        className={`[clip-path:inset(0_round_22%)] ${className}`}
      >
        <image
          href={imageUrl(icon.image)}
          width={24}
          height={24}
          preserveAspectRatio="xMidYMid slice"
        />
      </svg>
    );
  const { Icon, color } = iconLook(icon);
  return <Icon aria-hidden className={`${color} ${className}`} />;
}

/**
 * A repo's icon: the one the user chose, in a Project's shape (0033), or else its initials on a
 * tile in a color picked from its name. No Repo is a dashed square.
 */
export function RepoIcon({ repo }: { repo?: Repo }) {
  if (!repo || repo.scratch)
    return <SquareDashed aria-hidden className="size-4 shrink-0 text-faint-foreground" />;
  if (repo.icon) return <ProjectIcon icon={repo.icon} className="size-4 shrink-0" />;
  const palette = iconColors.slice(1);
  let hash = 0;
  for (let i = 0; i < repo.name.length; i++) hash = (hash * 31 + repo.name.charCodeAt(i)) | 0;
  const color = palette[Math.abs(hash) % palette.length]!.text;
  return (
    <span
      aria-hidden
      className={`grid size-4 shrink-0 place-items-center rounded-[4px] bg-current/15 text-[8.5px] leading-none font-bold ${color}`}
    >
      {/* Drawn from the attribute, so the tile adds nothing to the text around it. */}
      <span data-initials={initials(repo.name)} className="before:content-[attr(data-initials)]" />
    </span>
  );
}

// How long a pointer rests on a thread before its card shows. Moving to another thread while
// one shows switches at once.
const cardDelay = 450;

/** The Repos filter's choice: every repo, No Repo's threads, or one repo by host and id. */
type RepoFilter = "all" | "none" | `${string}/${string}`;
const filterKey = "parallax:repoFilter";
// "true" while the Projects section is collapsed.
const collapsedKey = "parallax:projectsCollapsed";

/** A sidebar setting kept in localStorage, or null while there is none or storage is off. */
function readStored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function saveStored(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage is off: the setting lasts until the window closes.
  }
}

/** One row of the list: a thread, or a Project, with its host and what it asks of the user. */
type Item = (
  | { kind: "thread"; thread: Thread }
  | { kind: "project"; project: Project; runs: AgentRun[] }
) & {
  key: string;
  host: Host;
  view: ThreadsView;
  repo?: Repo;
  attention: Attention;
  /** When it was last prompted, for the order. */
  at: string;
};

/**
 * Search, the Repos filter, a menu to create a Project or add a repository, and New thread, then
 * every host's Projects in a collapsible section, then their threads, each the most recently active first (0033). A thread row shows its repo, how long ago
 * it was prompted or what it asks of the user, its title, branch, and provider. Snoozed and
 * Archived threads sit under the list. Resting on a thread shows a card with where and how it runs.
 */
export function ThreadList({
  hosts,
  host,
  selection,
  onSelect,
  onOpenProject,
  onOpenSettings,
  onDelete,
  onNewThread,
}: ThreadListProps) {
  const newProject = useRef<HTMLDialogElement>(null);
  const addMenuId = useId();
  const addMenu = useRef<HTMLDivElement>(null);
  const addRepositoryDialog = useRef<HTMLDialogElement>(null);
  const deleteDialog = useRef<HTMLDialogElement>(null);
  const projectsId = useId();
  // The thread or Project the delete dialog asks about.
  const [toDelete, setToDelete] = useState<Item>();
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string>();
  const [actionError, setActionError] = useState<string>();
  const [query, setQuery] = useState("");
  const [filter, setFilterState] = useState<RepoFilter>(
    () => (readStored(filterKey) as RepoFilter | null) ?? "all",
  );
  const setFilter = (next: RepoFilter) => {
    setFilterState(next);
    saveStored(filterKey, next);
  };
  const [collapsed, setCollapsedState] = useState(() => readStored(collapsedKey) === "true");
  const setCollapsed = (next: boolean) => {
    setCollapsedState(next);
    saveStored(collapsedKey, String(next));
  };
  const [card, setCard] = useState<{ item: Item; top: number; left: number }>();
  const cardTimer = useRef<number>(undefined);
  // Snoozes end on time even with nothing else changing.
  const now = useMinute();

  const open = hosts.find((h) => h.host.id === host.id)?.view;
  const many = hosts.length > 1;
  const q = query.trim().toLowerCase();

  const items: Item[] = hosts.flatMap(({ host: h, view }) => {
    const { state } = view;
    const repoOf = (id: string) => state.repos.find((r) => r.id === id);
    const threads = state.threads.map((t): Item => {
      const run = state.runs[t.id];
      return {
        kind: "thread",
        thread: t,
        key: `${h.id}/${t.id}`,
        host: h,
        view,
        repo: repoOf(t.repo),
        attention: attentionOf(t, run, asksOf(state, t.id)),
        at: lastPrompt(t),
      };
    });
    const projects = state.projects.map((p): Item => {
      const runs = Object.values(state.runs).filter((r) => r.project === p.id);
      return {
        kind: "project",
        project: p,
        runs,
        key: `${h.id}/${p.id}`,
        host: h,
        view,
        repo: state.repos.find((r) => r.path === p.repoPath),
        attention: projectAttention(runs, (id) => asksOf(state, id)),
        at: p.updatedAt,
      };
    });
    return [...threads, ...projects];
  });

  const titleOf = (item: Item) =>
    item.kind === "project"
      ? item.project.name
      : (item.view.state.titles[item.thread.id] ?? "Thread");
  const inFilter = (item: Item) =>
    filter === "all" ||
    (filter === "none"
      ? !item.repo || item.repo.scratch
      : filter === `${item.host.id}/${item.repo?.id}`);
  const shown = items
    .filter((i) => inFilter(i) && (!q || titleOf(i).toLowerCase().includes(q)))
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  const isSnoozed = (i: Item) => i.kind === "thread" && snoozed(i.thread, i.attention, now);
  const isArchived = (i: Item) => i.kind === "thread" && !!i.thread.archived;
  const projects = shown.filter((i) => i.kind === "project");
  const threads = shown.filter((i) => i.kind === "thread" && !isArchived(i) && !isSnoozed(i));
  const snoozedItems = shown.filter((i) => !isArchived(i) && isSnoozed(i));
  const archived = shown.filter(isArchived);
  // The Projects section shows once any host has a Project, even while search or the filter hides
  // them all.
  const hasProjects = items.some((i) => i.kind === "project");

  // Mod+1 to Mod+9 open the first nine rows shown, which show their badges while Mod is held: the
  // Projects section's, unless it's collapsed, then the threads'.
  const listed = [...(collapsed ? [] : projects), ...threads];
  const modHeld = useModHeld();
  const openItem = (item: Item) =>
    onSelect(
      item.host.id,
      item.kind === "thread"
        ? { kind: "thread", threadId: item.thread.id }
        : { kind: "project", projectId: item.project.id },
    );
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const n = rowShortcut(e);
      const item = n === undefined ? undefined : listed[n];
      if (!item || document.querySelector("dialog[open]")) return;
      e.preventDefault();
      openItem(item);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  const showCard = (item: Item, row: HTMLElement) => {
    window.clearTimeout(cardTimer.current);
    const place = () => {
      const rect = row.getBoundingClientRect();
      // A little clear of the sidebar, and kept on screen: it is at most about 15rem tall.
      const edge = (row.closest("#sidebar") ?? row).getBoundingClientRect().right;
      setCard({ item, top: Math.min(rect.top, window.innerHeight - 248), left: edge + 12 });
    };
    if (card) place();
    else cardTimer.current = window.setTimeout(place, cardDelay);
  };
  const hideCard = () => {
    window.clearTimeout(cardTimer.current);
    setCard(undefined);
  };

  const askDelete = (item: Item) => {
    setToDelete(item);
    setDeleteError(undefined);
    deleteDialog.current?.showModal();
  };

  const row = (item: Item) => {
    const n = modHeld ? listed.indexOf(item) : -1;
    const badge = n >= 0 && n < 9 ? <RowBadge index={n} /> : undefined;
    const selected =
      item.host.id === host.id &&
      (item.kind === "thread"
        ? selection.kind === "thread" && selection.threadId === item.thread.id
        : selection.kind === "project" && selection.projectId === item.project.id);
    if (item.kind === "project")
      return (
        <ProjectRow
          key={item.key}
          project={item.project}
          runs={item.runs}
          attention={item.attention}
          host={many ? item.host : undefined}
          selected={selected}
          badge={badge}
          editable={item.view.editable}
          iconImageBytes={item.view.iconImageBytes}
          onOpen={() => openItem(item)}
          onUpdate={async (change) =>
            setActionError(await item.view.updateProject(item.project.id, change))
          }
          onDelete={item.view.deletable ? () => askDelete(item) : undefined}
        />
      );
    const { thread: t, view } = item;
    return (
      <ThreadRow
        key={item.key}
        thread={t}
        hostId={item.host.id}
        title={titleOf(item)}
        run={view.state.runs[t.id]}
        repo={item.repo}
        attention={item.attention}
        selected={selected}
        badge={badge}
        snoozable={view.attention}
        onOpen={() => openItem(item)}
        onArchive={async () => setActionError(await view.archive(t.id, !t.archived))}
        onSnooze={async (until) =>
          setActionError(await view.update(t.id, { snoozedUntil: until.toISOString() }))
        }
        onDelete={() => askDelete(item)}
        onRest={(el) => showCard(item, el)}
        onLeave={hideCard}
      />
    );
  };

  const addRepository = async () => {
    const path = await window.parallax.pickFolder();
    if (!path || !open) return;
    const repo = await open.addRepo(path);
    if (typeof repo === "string") return setActionError(repo);
    setActionError(undefined);
    onSelect(host.id, { kind: "new", groupId: repo.id });
  };

  const confirmDelete = async () => {
    if (!toDelete) return;
    setDeleting(true);
    // Running agents are stopped first, so this can take a moment. Deleting the open Project
    // leaves it once it's gone from the list (App.tsx).
    const error =
      toDelete.kind === "thread"
        ? await onDelete(toDelete.host.id, toDelete.thread)
        : await toDelete.view.removeProject(toDelete.project.id);
    setDeleting(false);
    if (error) setDeleteError(error);
    else deleteDialog.current?.close();
  };

  const errors = [...new Set([actionError, ...hosts.map((h) => h.view.error)].filter(Boolean))];

  return (
    <>
      <div className="flex items-center gap-1 px-2">
        <label className="flex min-w-0 flex-1 items-center gap-2 rounded-lg bg-hover px-2.5 py-1.5 focus-within:outline-2 focus-within:outline-ring">
          <Search aria-hidden className="size-4 shrink-0 text-faint-foreground" />
          <input
            type="search"
            aria-label="Search threads and Projects"
            placeholder="Search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
            className="min-w-0 flex-1 bg-transparent text-[13px] placeholder:text-faint-foreground focus-visible:outline-none"
          />
        </label>
        <RepoFilterMenu hosts={hosts} filter={filter} onFilter={setFilter} many={many} />
        <IconButton label="New project or repository" popoverTarget={addMenuId}>
          <FolderPlus />
        </IconButton>
        <div
          ref={addMenu}
          id={addMenuId}
          popover="auto"
          role="menu"
          aria-label="New project or repository"
          onToggle={(e: ToggleEvent<HTMLDivElement>) => {
            if (e.newState === "open")
              e.currentTarget.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
          }}
          onKeyDown={moveFocus}
          className={`${menuPanel("end")} min-w-40 p-1`}
        >
          {(
            [
              ["New project…", newProject],
              ["Add repository…", addRepositoryDialog],
            ] as const
          ).map(([label, dialog]) => (
            <button
              key={label}
              type="button"
              role="menuitem"
              className={menuItem}
              onClick={() => {
                addMenu.current?.hidePopover();
                dialog.current?.showModal();
              }}
            >
              {label}
            </button>
          ))}
        </div>
        <IconButton label="New thread" command="newThread" onClick={onNewThread}>
          <SquarePen />
        </IconButton>
      </div>
      <div onScroll={hideCard} className="mt-2 min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {errors.map((e) => (
          <p key={e} role="alert" className="px-2 pb-1 text-[12px] text-danger">
            {e}
          </p>
        ))}
        {hasProjects && (
          <>
            {/* The heading holds only its toggle, so its name is just "Projects". */}
            <div className={`${sectionHeading} pr-0`}>
              <h2 className="flex flex-1 self-stretch">
                <button
                  type="button"
                  aria-expanded={!collapsed}
                  aria-controls={projectsId}
                  onClick={() => setCollapsed(!collapsed)}
                  className="flex flex-1 items-center gap-1 text-left hover:text-foreground"
                >
                  Projects
                  <ChevronRight
                    aria-hidden
                    className={`size-3.5 transition-transform ${collapsed ? "" : "rotate-90"}`}
                  />
                </button>
              </h2>
              <IconButton label="New project" onClick={() => newProject.current?.showModal()}>
                <Plus />
              </IconButton>
            </div>
            <div id={projectsId} hidden={collapsed}>
              <ul aria-label="Projects" className="flex flex-col gap-0.5">
                {projects.map(row)}
              </ul>
              {projects.length === 0 && <p className={emptyNote}>Nothing matches</p>}
            </div>
            <h2 className={`${sectionHeading} mt-2`}>Threads</h2>
          </>
        )}
        <ul aria-label="Threads" className="flex flex-col gap-0.5">
          {threads.map(row)}
        </ul>
        {threads.length === 0 && (
          <p className={emptyNote}>
            {q || filter !== "all" ? "Nothing matches" : "No threads yet"}
          </p>
        )}
      </div>
      <Drawer label="Snoozed" items={snoozedItems} row={row} />
      <Drawer label="Archived" items={archived} row={row} />
      {card && (
        <ThreadCard
          title={titleOf(card.item)}
          run={
            card.item.kind === "thread" ? card.item.view.state.runs[card.item.thread.id] : undefined
          }
          repo={{
            name: card.item.repo && !card.item.repo.scratch ? card.item.repo.name : "No Repo",
            icon: <RepoIcon repo={card.item.repo} />,
          }}
          host={card.item.host}
          snoozedUntil={card.item.kind === "thread" ? card.item.thread.snoozedUntil : undefined}
          top={card.top}
          left={card.left}
        />
      )}
      <NewProjectDialog
        ref={newProject}
        hosts={hosts.map((h) => h.host)}
        hostId={host.id}
        repos={open?.state.repos ?? []}
        create={open?.createProject ?? (async () => "Not connected")}
        onCreated={(hostId, project) => onOpenProject(hostId, project.id)}
      />
      <AddRepositoryDialog
        ref={addRepositoryDialog}
        local={host.id === localId}
        onLocalFolder={() => void addRepository()}
      />
      <dialog
        ref={deleteDialog}
        aria-labelledby="delete-title"
        className="m-auto w-[24rem] rounded-xl border border-border bg-surface text-foreground shadow-composer backdrop:bg-black/50"
      >
        <form method="dialog" className="px-5 pt-4 pb-4">
          <h2 id="delete-title" className="text-[15px] font-semibold">
            {toDelete?.kind === "project" ? "Delete this Project?" : "Delete this thread?"}
          </h2>
          <p className="mt-1.5 text-[13px] text-muted-foreground">
            “{toDelete && titleOf(toDelete)}” goes for good
            {toDelete?.kind === "project"
              ? projectLoss(toDelete.runs.length)
              : ", with its transcript, worktree, and branch."}
          </p>
          {deleteError && (
            <p role="alert" className="mt-2 text-[12.5px] text-danger">
              {deleteError}
            </p>
          )}
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
              disabled={deleting}
              onClick={() => void confirmDelete()}
              className="rounded-md bg-red-600 px-3 py-1.5 text-[13px] font-medium text-white enabled:hover:opacity-90 disabled:opacity-50"
            >
              {deleting ? "Deleting…" : "Delete"}
            </button>
          </div>
        </form>
      </dialog>
      <div className="border-t border-border p-2">
        <ConnectionStatus hostId={host.id} />
        <Footer
          onOpenSettings={onOpenSettings}
          onOpenUsage={() => onSelect(host.id, { kind: "usage" })}
        />
      </div>
    </>
  );
}

/** The rest of Delete Project's warning, after "“name” goes for good", for a Project of `agents` runs. */
function projectLoss(agents: number): string {
  if (agents === 0) return ".";
  if (agents === 1)
    return ", with its agent's transcript, worktree, and branch. A running agent is stopped first.";
  return `, with its ${agents} agents' transcripts, worktrees, and branches. Running agents are stopped first.`;
}

/** `Date.now()`, again each minute. */
function useMinute() {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  return now;
}

/** A drawer pinned under the list, such as Snoozed or Archived, shown while it holds any rows. */
function Drawer<T>({
  label,
  items,
  row,
}: {
  label: string;
  items: T[];
  row: (item: T) => ReactNode;
}) {
  if (items.length === 0) return null;
  return (
    <details className="group/drawer max-h-[40%] shrink-0 overflow-y-auto px-2 pb-1">
      <summary className="flex cursor-default list-none items-center gap-3 rounded-md px-2 py-1.5 text-[12.5px] text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden">
        <span>
          {label} <span className="text-faint-foreground">({items.length})</span>
        </span>
        <span aria-hidden className="h-px flex-1 bg-border" />
        <ChevronDown
          aria-hidden
          className="size-4 shrink-0 transition-transform group-open/drawer:rotate-180"
        />
      </summary>
      <ul className="flex flex-col gap-0.5">{items.map(row)}</ul>
    </details>
  );
}

/**
 * The Repos filter (0033): a searchable menu of All repos, No repo, and each host's repositories,
 * each with its icon. Where the repo's plxd keeps icons, its gear opens the icon picker.
 */
function RepoFilterMenu({
  hosts,
  filter,
  onFilter,
  many,
}: {
  hosts: HostThreads[];
  filter: RepoFilter;
  onFilter: (filter: RepoFilter) => void;
  many: boolean;
}) {
  const menuId = useId();
  const pickerId = useId();
  const menu = useRef<HTMLDivElement>(null);
  const picker = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<{ view: ThreadsView; repo: Repo }>();
  const q = query.trim().toLowerCase();
  const repos = hosts.flatMap(({ host, view }) =>
    view.state.repos
      .filter((r) => !r.scratch && (!q || r.name.toLowerCase().includes(q)))
      .map((repo) => ({ host, view, repo })),
  );
  const current = repos.find((r) => filter === `${r.host.id}/${r.repo.id}`);
  const choose = (next: RepoFilter) => {
    onFilter(next);
    menu.current?.hidePopover();
  };
  const check = (on: boolean) => (
    <Check aria-hidden className={`ml-auto ${on ? "" : "invisible"}`} />
  );

  return (
    <>
      <button
        type="button"
        aria-label={`Repos: ${current?.repo.name ?? (filter === "none" ? "No repo" : "All repos")}`}
        title="Filter by repo"
        aria-pressed={filter !== "all"}
        popoverTarget={menuId}
        className="grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground aria-pressed:bg-selected aria-pressed:text-foreground [&_svg]:size-4"
      >
        {current ? (
          <RepoIcon repo={current.repo} />
        ) : filter === "none" ? (
          <SquareDashed />
        ) : (
          <ListFilter />
        )}
      </button>
      <div
        ref={menu}
        id={menuId}
        popover="auto"
        role="menu"
        aria-label="Repos"
        onToggle={(e: ToggleEvent<HTMLDivElement>) => {
          if (e.newState === "open") e.currentTarget.querySelector("input")?.focus();
          else setQuery("");
        }}
        onKeyDown={moveFocus}
        className={`${menuPanel("end")} w-64 p-1`}
      >
        <label className="mb-1 flex items-center gap-2 border-b border-border px-2 py-1.5">
          <Search aria-hidden className="size-3.5 shrink-0 text-faint-foreground" />
          <input
            aria-label="Search repos"
            placeholder="Search repos…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="min-w-0 flex-1 bg-transparent text-[13px] placeholder:text-faint-foreground focus-visible:outline-none"
          />
        </label>
        {!q && (
          <>
            <button
              type="button"
              role="menuitemradio"
              aria-checked={filter === "all"}
              className={menuItem}
              onClick={() => choose("all")}
            >
              <Folder aria-hidden />
              All repos
              {check(filter === "all")}
            </button>
            <button
              type="button"
              role="menuitemradio"
              aria-checked={filter === "none"}
              className={menuItem}
              onClick={() => choose("none")}
            >
              <SquareDashed aria-hidden />
              No repo
              {check(filter === "none")}
            </button>
          </>
        )}
        {repos.map(({ host, view, repo }) => {
          const key = `${host.id}/${repo.id}` as const;
          return (
            <div key={key} className="group/repo relative">
              <button
                type="button"
                role="menuitemradio"
                aria-checked={filter === key}
                className={`${menuItem} pr-8`}
                onClick={() => choose(key)}
              >
                <RepoIcon repo={repo} />
                <span className="min-w-0 truncate">{repo.name}</span>
                {many && <span className="shrink-0 text-faint-foreground">{host.name}</span>}
                {check(filter === key)}
              </button>
              {view.attention && (
                <button
                  type="button"
                  aria-label={`Change ${repo.name}'s icon`}
                  title="Change icon"
                  onClick={(e) => {
                    setEditing({ view, repo });
                    picker.current?.showPopover({ source: e.currentTarget });
                  }}
                  className="absolute top-1/2 right-1 grid size-6 -translate-y-1/2 place-items-center rounded-md text-faint-foreground opacity-0 group-hover/repo:opacity-100 hover:bg-hover hover:text-foreground focus-visible:opacity-100 [&_svg]:size-3.5"
                >
                  <Settings />
                </button>
              )}
            </div>
          );
        })}
        {q && repos.length === 0 && (
          <p className="px-2 py-1.5 text-[12.5px] text-faint-foreground">No repos match</p>
        )}
      </div>
      {/* Each pick saves at once; the picker stays open for the next. */}
      <IconPicker
        ref={picker}
        id={pickerId}
        value={editing?.repo.icon}
        onPick={(icon) => editing && void editing.view.updateRepo(editing.repo.id, icon)}
        maxImageBytes={editing?.view.iconImageBytes}
      />
    </>
  );
}

/**
 * The footer's buttons: Profile, which opens Settings > Account and shows the account's picture or
 * initials (0037), Settings, Usage, and Update when `updatable` (Update.tsx).
 */
function Footer({
  onOpenSettings,
  onOpenUsage,
}: Pick<ThreadListProps, "onOpenSettings"> & { onOpenUsage: () => void }) {
  const profile = useProfile();
  return (
    <div className="flex items-center gap-1">
      <IconButton
        label={profile ? profile.name || profile.email : "Profile"}
        onClick={() => onOpenSettings("account")}
      >
        {profile ? <Avatar profile={profile} size={22} /> : <CircleUser />}
      </IconButton>
      <IconButton label="Settings" command="settings" onClick={() => onOpenSettings("general")}>
        <Settings />
      </IconButton>
      <IconButton label="Usage" onClick={onOpenUsage}>
        <ChartNoAxesColumn />
      </IconButton>
      {window.parallax.updatable && <UpdateButton />}
    </div>
  );
}

/**
 * A Project's row, one line for its whole coordinator and subagents: its icon and name, then what
 * its runs ask of the user or its age. Its tooltip counts its agents, and names its host when there
 * are several. Where its host's plxd can edit or delete Projects, hovering or focusing it swaps the
 * status for its actions, which also open by right-clicking the row: Rename, which edits the name
 * in place, Change icon, which opens the icon picker under the row's icon, and Delete….
 */
function ProjectRow({
  project,
  runs,
  attention,
  host,
  selected,
  badge,
  editable,
  iconImageBytes,
  onOpen,
  onUpdate,
  onDelete,
}: {
  project: Project;
  runs: AgentRun[];
  attention: Attention;
  /** Its host, named when there is more than one. */
  host?: Host;
  selected: boolean;
  /** Its Mod+number badge, shown in place of its status while Mod is held. */
  badge?: ReactNode;
  editable: boolean;
  /** Its host's cap on an icon image, where its plxd keeps them. */
  iconImageBytes?: number;
  onOpen: () => void;
  /** Sends `project/update`, and settles once plxd has answered. */
  onUpdate: (change: ProjectChange) => Promise<void>;
  /** Asks to delete it, where its host's plxd can (`projectDelete`). */
  onDelete?: () => void;
}) {
  const menuId = useId();
  const pickerId = useId();
  const menu = useRef<HTMLDivElement>(null);
  const actions = useRef<HTMLButtonElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const iconSpot = useRef<HTMLSpanElement>(null);
  const picker = useRef<HTMLDivElement>(null);
  // The name the field opened with, while Rename is open.
  const [renaming, setRenaming] = useState<string>();
  // The new name, shown until plxd answers.
  const [saving, setSaving] = useState<string>();
  // Set while the name box is open, so Enter and the blur that follows save once.
  const editing = useRef(false);
  // After Enter or Escape, the row takes focus back from the name box.
  const refocus = useRef(false);
  useEffect(() => {
    if (renaming !== undefined || !refocus.current) return;
    refocus.current = false;
    button.current?.focus();
  }, [renaming]);

  const choose = (action: () => void) => () => {
    menu.current?.hidePopover();
    action();
  };
  const startRename = () => {
    editing.current = true;
    setRenaming(project.name);
  };
  // An empty name, or the one the field opened with, saves nothing, even if another client has
  // renamed the Project since.
  const endRename = async (name: string | undefined, focusRow: boolean) => {
    if (!editing.current) return;
    editing.current = false;
    refocus.current = focusRow;
    setRenaming(undefined);
    const next = name?.trim();
    if (!next || next === renaming) return;
    setSaving(next);
    await onUpdate({ name: next });
    setSaving(undefined);
  };

  const icon = <ProjectIcon icon={project.icon} className="size-4" />;
  const actionable = editable || !!onDelete;
  const tooltip =
    [runs.length > 0 && `${runs.length} ${runs.length === 1 ? "agent" : "agents"}`, host?.name]
      .filter(Boolean)
      .join(" · ") || undefined;
  return (
    <li data-kind="project" className="group/row relative">
      {renaming !== undefined ? (
        <div
          className={`${row} ${selected ? current : ""} outline-2 -outline-offset-2 outline-ring`}
        >
          <span className="grid shrink-0 place-items-center">{icon}</span>
          <input
            aria-label="Project name"
            defaultValue={renaming}
            autoFocus
            spellCheck={false}
            autoComplete="off"
            onFocus={(e) => e.currentTarget.select()}
            onKeyDown={(e) => {
              // Enter and Escape belong to an input method while it composes.
              if (e.nativeEvent.isComposing) return;
              if (e.key === "Enter") void endRename(e.currentTarget.value, true);
              else if (e.key === "Escape") void endRename(undefined, true);
            }}
            onBlur={(e) => void endRename(e.currentTarget.value, false)}
            className="min-w-0 flex-1 bg-transparent text-[13px] text-foreground focus-visible:outline-none"
          />
        </div>
      ) : (
        <button
          ref={button}
          type="button"
          aria-current={selected ? "page" : undefined}
          title={tooltip}
          onClick={onOpen}
          onContextMenu={actionable ? (e) => openOnContextMenu(e, actions.current) : undefined}
          className={`${row} ${selected ? current : ""} ${actionable ? "group-has-[:focus-visible]/row:pr-8 group-hover/row:pr-8" : ""}`}
        >
          <span ref={iconSpot} data-project-icon className="grid shrink-0 place-items-center">
            {icon}
          </span>
          <span
            data-title
            className={`min-w-0 flex-1 truncate ${selected ? "text-foreground" : "text-foreground/80"}`}
          >
            {saving ?? project.name}
          </span>
          <span
            data-status
            className={`shrink-0 text-[11.5px] text-faint-foreground ${actionable ? "group-has-[:focus-visible]/row:hidden group-hover/row:hidden" : ""}`}
          >
            {badge ??
              (attention === "settled" ? (
                age(project.updatedAt)
              ) : (
                <AttentionBadge
                  attention={attention}
                  count={runs.filter((r) => isRunning(r.status)).length}
                />
              ))}
          </span>
        </button>
      )}
      {actionable && renaming === undefined && (
        <>
          <div className="absolute top-1/2 right-1 -translate-y-1/2 opacity-0 group-has-[:focus-visible]/row:opacity-100 group-hover/row:opacity-100">
            <button
              ref={actions}
              type="button"
              aria-label="Project actions"
              title="Project actions"
              popoverTarget={menuId}
              className="grid size-5.5 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-4"
            >
              <Ellipsis />
            </button>
          </div>
          <div
            ref={menu}
            id={menuId}
            popover="auto"
            role="menu"
            aria-label="Project actions"
            onToggle={(e: ToggleEvent<HTMLDivElement>) => {
              if (e.newState === "open")
                e.currentTarget.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
            }}
            onKeyDown={moveFocus}
            className={`${menuPanel("end")} min-w-36 p-1`}
          >
            {editable && (
              <>
                <button
                  type="button"
                  role="menuitem"
                  className={menuItem}
                  onClick={choose(startRename)}
                >
                  Rename
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className={menuItem}
                  onClick={choose(() =>
                    picker.current?.showPopover({ source: iconSpot.current ?? undefined }),
                  )}
                >
                  Change icon
                </button>
              </>
            )}
            {onDelete && (
              <button
                type="button"
                role="menuitem"
                className={`${menuItem} text-danger`}
                onClick={choose(onDelete)}
              >
                Delete…
              </button>
            )}
          </div>
        </>
      )}
      {/* Each pick saves at once; the picker stays open for the next. */}
      {editable && (
        <IconPicker
          ref={picker}
          id={pickerId}
          value={project.icon}
          onPick={(next) => void onUpdate({ icon: next })}
          maxImageBytes={iconImageBytes}
        />
      )}
    </li>
  );
}

/** Each backend's logo, by its name in `AgentRun.backend`. */
export const backendLogos: Partial<Record<string, ComponentType<SVGProps<SVGSVGElement>>>> = {
  claude: ClaudeLogo,
  codex: OpenAILogo,
  cursor: CursorLogo,
};

/**
 * A thread row's first line: its repo's icon and name, then `status`: what it asks of the user, or
 * how long ago it was prompted. Hovering or focusing the row hides the status and keeps the repo
 * name clear of the row's actions.
 */
function RowHead({ repo, status }: { repo?: Repo; status: ReactNode }) {
  return (
    <span className="flex w-full items-center gap-1.5 text-[12px] text-faint-foreground group-has-[:focus-visible]/row:pr-34 group-hover/row:pr-34">
      <RepoIcon repo={repo} />
      <span className="min-w-0 truncate">{repo && !repo.scratch ? repo.name : "No repo"}</span>
      <span
        data-status
        className="ml-auto shrink-0 text-[11.5px] group-has-[:focus-visible]/row:hidden group-hover/row:hidden"
      >
        {status}
      </span>
    </span>
  );
}

/**
 * A thread's row (0033): its repo and status (what it asks of the user, or how long ago it was
 * prompted), its title, then its branch, diff, and provider. Resting on it shows its card;
 * hovering or focusing it swaps the status for Snooze, Archive, and more actions, which also open
 * by right-clicking the row: native popovers, so Escape and clicking away close them.
 */
function ThreadRow({
  thread,
  hostId,
  title,
  run,
  repo,
  attention,
  selected,
  badge,
  snoozable,
  onOpen,
  onArchive,
  onSnooze,
  onDelete,
  onRest,
  onLeave,
}: {
  thread: Thread;
  /** Its host, which a drag onto the composer names (PLX-378). */
  hostId: string;
  title: string;
  run?: AgentRun;
  repo?: Repo;
  attention: Attention;
  selected: boolean;
  /** Its Mod+number badge, shown in place of its status while Mod is held. */
  badge?: ReactNode;
  /** Whether its plxd keeps seen and snooze state (`threadAttention`). */
  snoozable: boolean;
  onOpen: () => void;
  onArchive: () => void;
  onSnooze: (until: Date) => void;
  onDelete: () => void;
  onRest: (row: HTMLElement) => void;
  onLeave: () => void;
}) {
  const menuId = useId();
  const snoozeId = useId();
  const menu = useRef<HTMLDivElement>(null);
  const snoozeMenu = useRef<HTMLDivElement>(null);
  const actions = useRef<HTMLButtonElement>(null);
  const [custom, setCustom] = useState("");
  const choose = (action: () => void) => () => {
    menu.current?.hidePopover();
    snoozeMenu.current?.hidePopover();
    onLeave();
    action();
  };
  const Logo = run?.backend ? backendLogos[run.backend] : undefined;
  const hasDetails = !!(run?.branch || run?.diff || Logo);
  const snoozedNow = !!thread.snoozedUntil && Date.parse(thread.snoozedUntil) > Date.now();
  const mainLabel = (
    <>
      {thread.archived ? <ArchiveRestore aria-hidden /> : <Archive aria-hidden />}
      {thread.archived ? "Unarchive" : "Archive"}
    </>
  );
  const status =
    badge ??
    (attention === "settled" ? (
      age(lastPrompt(thread))
    ) : (
      <AttentionBadge attention={attention} since={lastPrompt(thread)} />
    ));
  return (
    <li
      data-kind="thread"
      className="group/row relative"
      onMouseEnter={(e) => onRest(e.currentTarget)}
      onMouseLeave={onLeave}
    >
      <button
        type="button"
        aria-current={selected ? "page" : undefined}
        onClick={onOpen}
        onContextMenu={(e) => {
          onLeave();
          openOnContextMenu(e, actions.current);
        }}
        // Dropped on the composer, it attaches the thread to the message (PLX-378).
        draggable
        onDragStart={(e) => {
          onLeave();
          dragThread(e.dataTransfer, hostId, thread.id);
        }}
        className={`flex w-full flex-col gap-0.5 rounded-lg px-2 py-1.5 text-left hover:bg-hover ${selected ? current : ""}`}
      >
        <RowHead repo={repo} status={status} />
        <span
          data-title
          className={`w-full truncate text-[13px] ${selected || attention !== "settled" ? "text-foreground" : "text-foreground/70"}`}
        >
          {title}
        </span>
        {hasDetails && (
          <span className="flex w-full items-center gap-2 text-[11.5px] text-faint-foreground">
            <span className="min-w-0 flex-1 truncate">{run?.branch}</span>
            {run?.diff && (
              <span className="shrink-0 tabular-nums">
                <span className="text-added">+{run.diff.insertions}</span>{" "}
                <span className="text-danger">−{run.diff.deletions}</span>
              </span>
            )}
            {Logo && <Logo className="size-3.5 shrink-0" />}
          </span>
        )}
      </button>
      <div className="absolute top-1 right-1 flex items-center gap-0.5 opacity-0 group-has-[:focus-visible]/row:opacity-100 group-hover/row:opacity-100">
        {snoozable && (
          <button
            type="button"
            aria-label={snoozedNow ? "Snoozed" : "Snooze"}
            title={snoozedNow ? `Snoozed until ${clock(thread.snoozedUntil!)}` : "Snooze"}
            popoverTarget={snoozeId}
            onClick={onLeave}
            className="grid size-5.5 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-3.5"
          >
            <AlarmClock />
          </button>
        )}
        <button
          type="button"
          onClick={() => {
            onLeave();
            onArchive();
          }}
          className="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-3.5"
        >
          {mainLabel}
        </button>
        <button
          ref={actions}
          type="button"
          aria-label="Thread actions"
          title="Thread actions"
          popoverTarget={menuId}
          className="grid size-5.5 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground [&_svg]:size-4"
        >
          <Ellipsis />
        </button>
      </div>
      {snoozable && (
        <div
          ref={snoozeMenu}
          id={snoozeId}
          popover="auto"
          role="menu"
          aria-label="Snooze"
          onToggle={(e: ToggleEvent<HTMLDivElement>) => {
            if (e.newState === "open")
              e.currentTarget.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
          }}
          onKeyDown={moveFocus}
          className={`${menuPanel("end")} min-w-56 p-1`}
        >
          {snoozeChoices().map((c) => (
            <button
              key={c.label}
              type="button"
              role="menuitem"
              className={menuItem}
              onClick={choose(() => onSnooze(c.until))}
            >
              {c.label}
              <span className="ml-auto text-[12px] text-faint-foreground tabular-nums">
                {c.label.startsWith("In") ? clock(c.until.toISOString()) : when(c.until)}
              </span>
            </button>
          ))}
          <div className="my-1 h-px bg-border" />
          <form
            className="flex items-center gap-1 px-1 py-0.5"
            onSubmit={(e) => {
              e.preventDefault();
              const until = new Date(custom);
              if (!Number.isNaN(until.getTime())) choose(() => onSnooze(until))();
            }}
          >
            <input
              type="datetime-local"
              aria-label="Snooze until"
              value={custom}
              onChange={(e) => setCustom(e.target.value)}
              className="min-w-0 flex-1 rounded-md bg-hover px-1.5 py-1 text-[12px] [color-scheme:inherit]"
            />
            <button
              type="submit"
              disabled={!custom}
              className="rounded-md px-2 py-1 text-[12px] hover:bg-hover disabled:opacity-50"
            >
              Custom
            </button>
          </form>
          {snoozedNow && (
            <button
              type="button"
              role="menuitem"
              className={menuItem}
              onClick={choose(() => onSnooze(new Date()))}
            >
              Unsnooze
            </button>
          )}
        </div>
      )}
      <div
        ref={menu}
        id={menuId}
        popover="auto"
        role="menu"
        aria-label="Thread actions"
        onToggle={(e: ToggleEvent<HTMLDivElement>) => {
          if (e.newState === "open")
            e.currentTarget.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
        }}
        onKeyDown={moveFocus}
        className={`${menuPanel("end")} min-w-36 p-1`}
      >
        <button type="button" role="menuitem" className={menuItem} onClick={choose(onArchive)}>
          {thread.archived ? "Unarchive" : "Archive"}
        </button>
        <button
          type="button"
          role="menuitem"
          className={`${menuItem} text-danger`}
          onClick={choose(onDelete)}
        >
          Delete…
        </button>
      </div>
    </li>
  );
}

/** A day and time a snooze ends, as its menu writes it: "Mon 9:00 AM". */
function when(date: Date): string {
  return date.toLocaleString(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" });
}

/** A run's status as its icon and color, in a thread's card and the Agents list. */
export const statusLooks: Partial<Record<AgentStatus, { Icon: LucideIcon; color: string }>> = {
  starting: { Icon: LoaderCircle, color: "text-emerald-500 [&_svg]:animate-spin" },
  running: { Icon: LoaderCircle, color: "text-emerald-500 [&_svg]:animate-spin" },
  completed: { Icon: CircleCheck, color: "text-muted-foreground" },
  failed: { Icon: CircleAlert, color: "text-danger" },
  cancelled: { Icon: CircleSlash, color: "text-muted-foreground" },
  interrupted: { Icon: CirclePause, color: "text-amber-500" },
  accepted: { Icon: GitMerge, color: "text-violet-500" },
};

/**
 * What a thread is, shown beside its row: its title, repository, computer, branch, account,
 * changes, and status, from its run as last listed (resting on the row lists it again).
 */
function ThreadCard({
  title,
  run,
  repo,
  host,
  snoozedUntil,
  top,
  left,
}: {
  title: string;
  run?: AgentRun;
  repo: { name: string; icon: ReactNode };
  host: Host;
  snoozedUntil?: string;
  top: number;
  left: number;
}) {
  const Logo = run?.backend ? backendLogos[run.backend] : undefined;
  const look = run && (statusLooks[run.status] ?? statusLooks.completed!);
  return (
    <div
      role="tooltip"
      style={{ top, left }}
      className="pointer-events-none fixed z-50 w-64 rounded-lg border border-border bg-surface p-3 text-foreground shadow-composer"
    >
      <p className="line-clamp-2 text-[13px] font-medium">{title}</p>
      <ul className="mt-2 flex flex-col gap-1.5 text-[12.5px] text-muted-foreground [&_svg]:size-3.5 [&_svg]:shrink-0 [&>li]:flex [&>li]:min-w-0 [&>li]:items-center [&>li]:gap-2">
        <li>
          {repo.icon}
          <span className="truncate">{repo.name}</span>
        </li>
        <li>
          {host.destination ? <Server /> : <Laptop />}
          <span className="truncate">{host.name}</span>
        </li>
        {run?.branch && (
          <li>
            <GitBranch />
            <span className="truncate">{run.branch}</span>
          </li>
        )}
        {run?.accountId && (
          <li>
            {Logo ? <Logo /> : <Bot />}
            <span className="truncate">{accountLabel(run.accountId)}</span>
          </li>
        )}
        {run?.diff && (
          <li>
            <FileDiff />
            <span className="tabular-nums">
              {run.diff.files} {run.diff.files === 1 ? "file" : "files"}{" "}
              <span className="text-added">+{run.diff.insertions}</span>{" "}
              <span className="text-danger">−{run.diff.deletions}</span>
            </span>
          </li>
        )}
        {snoozedUntil && Date.parse(snoozedUntil) > Date.now() && (
          <li>
            <AlarmClock />
            <span className="truncate">Snoozed until {clock(snoozedUntil)}</span>
          </li>
        )}
        {run && look && (
          <li className={look.color}>
            <look.Icon />
            <span className="line-clamp-2">
              {runStatusLabel(run.status)}
              {run.status === "failed" && run.error && `: ${run.error}`}
            </span>
          </li>
        )}
      </ul>
    </div>
  );
}

/** How long ago, as the sidebar writes it: "now", "12m", "3h", "2d", "1w". */
export function age(time: string, now = Date.now()): string {
  const minutes = Math.floor((now - Date.parse(time)) / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return days < 7 ? `${days}d` : `${Math.floor(days / 7)}w`;
}

/** Each Settings section's name and icon, in the nav's order. */
const sections: { id: SettingsSection; name: string; Icon: LucideIcon }[] = [
  { id: "account", name: "Account", Icon: CircleUser },
  { id: "general", name: "General", Icon: Settings },
  { id: "appearance", name: "Appearance", Icon: Palette },
  { id: "keybinds", name: "Keybinds", Icon: Keyboard },
  { id: "providers", name: "Providers", Icon: Bot },
  { id: "sourceControl", name: "Source control", Icon: GitBranch },
  { id: "storage", name: "Storage", Icon: HardDrive },
  { id: "connections", name: "Connections", Icon: Server },
];
export const settingsNames = Object.fromEntries(sections.map((s) => [s.id, s.name])) as Record<
  SettingsSection,
  string
>;

/** The sidebar while Settings is open. */
export function SettingsNav({
  section,
  onSection,
  onBack,
}: {
  section: SettingsSection;
  onSection: (section: SettingsSection) => void;
  onBack: () => void;
}) {
  return (
    <div className="flex flex-col gap-px px-2">
      <button type="button" onClick={onBack} className={`${row} mb-3 text-muted-foreground`}>
        <ArrowLeft className="size-4" />
        Back to app
      </button>
      {sections.map((s) => (
        <button
          key={s.id}
          type="button"
          aria-current={s.id === section ? "page" : undefined}
          onClick={() => onSection(s.id)}
          className={`${row} ${s.id === section ? current : "text-foreground/80"}`}
        >
          <s.Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          {s.name}
        </button>
      ))}
    </div>
  );
}
