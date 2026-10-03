import { PanelBottom, PanelLeftOpen, PanelRight, Workflow } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { Thread } from "../protocol/generated/protocol";
import { Actions, type RepoAction } from "./Actions";
import { AgentChat } from "./AgentChat";
import type { Asked } from "./Approval";
import { useConnection } from "./ConnectionStatus";
import { ContextPanel } from "./ContextPanel";
import { FilesPanel } from "./FilesPanel";
import { GitMenu } from "./GitMenu";
import { NewThread } from "./NewThread";
import { NewThreadPicker } from "./NewThreadPicker";
import { localId, useHosts } from "./hosts";
import { iconImageBytes } from "./images";
import { OpenMenu } from "./OpenMenu";
import { AgentsPanel, useProjectAgents } from "./ProjectAgents";
import { ProjectChat } from "./ProjectChat";
import { PullRequestChip, PullRequestList, PullRequestView, usePullRequests } from "./PullRequests";
import { Settings } from "./Settings";
import { SidePanel } from "./SidePanel";
import { attentionOf } from "./attention";
import { useSnoozeAlarms } from "./alarms";
import { ProjectIcon, RepoIcon, SettingsNav, settingsNames, Sidebar, ThreadList } from "./Sidebar";
import { useThemePreference } from "./theme";
import { useAppearanceEffects } from "./appearance";
import { folderOf, runInDrawer, TerminalDrawer, TerminalPool, useDeleted } from "./ThreadTerminal";
import {
  asksOf,
  groupOf,
  groupThreads,
  idleThreads,
  noRepo,
  titleOf,
  useThreads,
  type ThreadsView,
} from "./threads";
import { isRunning } from "./transcript";
import { appShortcut, Breadcrumb, IconButton, TopBar, type Crumb } from "./ui";
import { UsagePage } from "./UsagePage";

/**
 * The main pane: a Project's coordinator chat, or with `agentId` one of its subagents' chats, a
 * thread (its id is its run's; `started` when New Thread just started it, until anything else is
 * selected), a new thread in a sidebar group (`threads.ts`; with no group, it's the first
 * repository's), or Usage.
 */
export type Selection =
  | { kind: "project"; projectId: string; agentId?: string }
  | { kind: "thread"; threadId: string; started?: boolean }
  | { kind: "new"; groupId?: string }
  | { kind: "usage" };

export type SettingsSection =
  | "account"
  | "general"
  | "appearance"
  | "keybinds"
  | "providers"
  | "sourceControl"
  | "storage"
  | "connections";

/**
 * The app frame: sidebar, then the chat or Settings, then the side panel.
 * Views are plain state, not routes: there is nothing to deep-link yet.
 */
export function App() {
  const [theme, setTheme] = useThemePreference();
  useAppearanceEffects();
  const hosts = useHosts();
  const [hostId, setHostId] = useState(localId);
  // The open host, or this computer once the open one is removed.
  const host = hosts.find((h) => h.id === hostId) ?? hosts[0]!;
  const [selection, setSelection] = useState<Selection>({ kind: "new" });
  // A Project just created on another host, opened once that host is open and lists it: the
  // check below drops a Project selection the open host's list doesn't have.
  const [opening, setOpening] = useState<{ hostId: string; projectId: string }>();
  const [settings, setSettings] = useState<SettingsSection | null>(null);
  // The host Set up GitHub opened Source control on (PLX-423).
  const [settingsHost, setSettingsHost] = useState<string>();
  const openSettings = (section: SettingsSection, hostId?: string) => {
    setSettings(section);
    setSettingsHost(hostId);
  };
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [panelOpen, setPanelOpen] = useState(false);
  const [panelExpanded, setPanelExpanded] = useState(false);
  // The folders whose terminal drawer is open, by key (ThreadTerminal.tsx).
  const [drawers, setDrawers] = useState<ReadonlySet<string>>(new Set());
  // The page a repository action last opened in the side panel's Browser view.
  const [browse, setBrowse] = useState<{ url: string }>();
  // A quiet note for the thread New Thread just started, such as the account it picked.
  const [notice, setNotice] = useState<{ threadId: string; text: string }>();
  // The pull request the side panel last opened, or with no URL its Pull requests view.
  const [showPr, setShowPr] = useState<{ url?: string }>();
  // A message the PR view handed the open thread's chat, until the chat takes it.
  const [compose, setCompose] = useState<{ text: string; send: boolean }>();
  const composed = useCallback(() => setCompose(undefined), []);

  const connection = useConnection(host.id);
  const connected = connection?.status === "connected";
  // Runs started here forward their permission requests, only to a plxd that takes the flag.
  const approvals = connected && "approvals" in connection.capabilities;
  // Every host's threads and Projects, loaded side by side for the sidebar's one list (0033).
  const [views, setViews] = useState<Readonly<Record<string, ThreadsView>>>({});
  const report = useCallback(
    (id: string, view: ThreadsView) =>
      setViews((prev) => (prev[id] === view ? prev : { ...prev, [id]: view })),
    [],
  );
  const threads = views[host.id] ?? idleThreads;
  const listed = useMemo(
    () => hosts.map((h) => ({ host: h, view: views[h.id] ?? idleThreads })),
    [hosts, views],
  );
  const { groups } = groupThreads(threads.state);
  // Every thread and Project the open host has listed, so a selection is dropped only once what it
  // opened leaves the list: a just-started one reaches the list a render after it opens.
  const known = useRef(new Set<string>());
  for (const t of threads.state.threads) known.current.add(t.id);
  for (const p of threads.state.projects) known.current.add(p.id);
  // The open thread's group (No Repo's until plxd lists it), or the new thread's.
  let group = groups[0]!;
  if (selection.kind === "thread") {
    const open = threads.state.threads.find((t) => t.id === selection.threadId);
    // Deleted, maybe by another client: leave it rather than show a stale transcript.
    if (!open && known.current.has(selection.threadId)) setSelection({ kind: "new" });
    const id = open ? groupOf(threads.state, open) : noRepo;
    group = groups.find((g) => g.id === id)!;
  } else if (selection.kind === "new")
    group = groups.find((g) => g.id === selection.groupId) ?? group;

  if (
    opening?.hostId === host.id &&
    threads.state.projects.some((p) => p.id === opening.projectId)
  ) {
    setOpening(undefined);
    setSelection({ kind: "project", projectId: opening.projectId });
  }
  // The open Project. Once its host is removed, the next host's list doesn't have it.
  const project =
    selection.kind === "project"
      ? threads.state.projects.find((p) => p.id === selection.projectId)
      : undefined;
  if (selection.kind === "project" && !project && known.current.has(selection.projectId))
    setSelection({ kind: "new" });
  const agents = useProjectAgents(host.id, project?.id, connected, approvals);
  // The open subagent, whose chat takes the coordinator's place while the Project stays selected.
  const agentId = selection.kind === "project" ? selection.agentId : undefined;
  const agent = agents.runs.find((r) => r.id === agentId);
  // The run whose folder the side panel's Files view browses: the open thread or subagent.
  const filesRunId = selection.kind === "thread" ? selection.threadId : agentId;
  // The open thread's linked pull requests, on a plxd that links them (PLX-318).
  const threadRun =
    selection.kind === "thread" ? threads.state.runs[selection.threadId] : undefined;
  const linksPrs = connected && "pullRequests" in connection.capabilities;
  const prs = usePullRequests(
    host.id,
    threadRun?.id,
    (linksPrs && threadRun?.pullRequests) || [],
    connected && "prDiff" in connection.capabilities,
  );
  // A PR action that fails for a missing or signed-out gh offers this, on a plxd that sets it up.
  const setUpGithub =
    connected && "githubSetup" in connection.capabilities
      ? () => openSettings("sourceControl", host.id)
      : undefined;
  const openPr = (url?: string) => {
    setPanelOpen(true);
    setShowPr({ url });
  };
  // Shrinks an expanded side panel, which hides the main pane the chat opens in.
  const openAgent = (id?: string) => {
    if (!project) return;
    setSelection({ kind: "project", projectId: project.id, agentId: id });
    setPanelExpanded(false);
  };
  // The permission requests the Project's other runs wait on, pinned in whichever of its chats is
  // open, each named and with a way to its own chat (RYA-196).
  const othersAsked = (open: string | undefined): Asked[] =>
    agents.runs.flatMap((run) => {
      if (run.id === open || !project) return [];
      const coordinator = run.id === project.coordinator;
      const from = {
        label: coordinator ? "Coordinator" : `Subagent: ${titleOf(run)}`,
        open: () => openAgent(coordinator ? undefined : run.id),
      };
      return (agents.waiting[run.id] ?? []).map((approval) => ({ runId: run.id, approval, from }));
    });

  // An open thread that has news is seen now, including one that finishes while it is open.
  const openThread =
    selection.kind === "thread"
      ? threads.state.threads.find((t) => t.id === selection.threadId)
      : undefined;
  const openNews =
    openThread &&
    threads.attention &&
    ["done", "failed"].includes(
      attentionOf(
        openThread,
        threads.state.runs[openThread.id],
        asksOf(threads.state, openThread.id),
      ),
    );
  useEffect(() => {
    if (openNews && openThread) void threads.update(openThread.id, { seen: true });
  }, [openNews, openThread, threads]);

  const openOnHost = (hostId: string, next: Selection) => {
    setSettings(null);
    setHostId(hostId);
    setSelection(next);
    setOpening(undefined);
  };
  useSnoozeAlarms(listed, (hostId, threadId) => openOnHost(hostId, { kind: "thread", threadId }));

  // The Project or repository crumb wears its sidebar icon. Under a subagent, the Project's goes
  // back to the coordinator. In a thread, the repository's opens New thread on that repository.
  let crumbs: Crumb[];
  if (project) {
    crumbs = [
      { label: host.name },
      {
        label: project.name,
        icon: <ProjectIcon icon={project.icon} />,
        onClick: agentId ? () => openAgent() : undefined,
      },
    ];
    if (agentId) crumbs.push({ label: agent ? titleOf(agent) : "Subagent", icon: <Workflow /> });
  } else {
    const repo = {
      label: group.name,
      icon: <RepoIcon repo={threads.state.repos.find((r) => r.id === group.id)} />,
      onClick:
        selection.kind === "thread"
          ? () => setSelection({ kind: "new", groupId: group.id })
          : undefined,
    };
    const page =
      selection.kind === "thread"
        ? (threads.state.titles[selection.threadId] ?? "Thread")
        : "New thread";
    crumbs = [{ label: host.name }, repo, { label: page }];
  }

  // A Project created on the open host is in its list already. Another host's list loads once
  // that host is open.
  const openProject = (hostId: string, projectId: string) => {
    if (hostId === host.id) return setSelection({ kind: "project", projectId });
    setHostId(hostId);
    setSelection({ kind: "new" });
    setOpening({ hostId, projectId });
  };

  const newThread = (groupId = selection.kind === "project" ? undefined : group.id) => {
    setSettings(null);
    setSelection({ kind: "new", groupId });
  };
  // Mod+N's picker of the repository a new thread goes in.
  const picker = useRef<HTMLDialogElement>(null);
  const deleteThread = async (hostId: string, thread: Thread) => {
    const view = views[hostId] ?? idleThreads;
    const error = await view.remove(thread);
    if (!error && selection.kind === "thread" && selection.threadId === thread.id)
      setSelection({ kind: "new", groupId: groupOf(view.state, thread) });
    return error;
  };

  const offline =
    connection?.status === "failed"
      ? "Disconnected from plxd"
      : connected
        ? undefined
        : "Connecting to plxd…";

  // Where the open thread's or New thread's terminals open (folderOf).
  const folder =
    settings || selection.kind === "usage" || selection.kind === "project"
      ? undefined
      : folderOf(
          host.id,
          threads.state,
          selection.kind === "thread" ? { threadId: selection.threadId } : { repoId: group.id },
        );
  const deleted = useDeleted(views);
  const drawerOpen = !!folder && drawers.has(folder.key);
  const toggleDrawer = () => {
    if (!folder) return;
    const next = new Set(drawers);
    if (!next.delete(folder.key)) next.add(folder.key);
    setDrawers(next);
  };
  // Runs a repository action in the folder's drawer, opened for it, and opens its preview.
  const runAction = (action: RepoAction) => {
    if (!folder) return;
    if (!drawers.has(folder.key)) setDrawers(new Set(drawers).add(folder.key));
    runInDrawer(folder, action.command);
    if (action.openPreview && action.previewUrl) {
      setPanelOpen(true);
      setBrowse({ url: action.previewUrl });
    }
  };

  // The app's shortcuts (ui.tsx), but Open, which OpenMenu takes, and Mod+1 to Mod+9, which
  // ThreadList takes.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const command = appShortcut(e);
      // A new thread waits for an open dialog, the picker included, to close.
      const dialog = !!document.querySelector("dialog[open]");
      if (command === "panel") setPanelOpen((open) => !open);
      else if (command === "sidebar") setSidebarOpen((open) => !open);
      else if (command === "terminal" && folder) toggleDrawer();
      else if (command === "newThread") {
        if (!dialog) picker.current?.showModal();
      } else if (command === "noRepoThread") {
        if (!dialog) newThread(noRepo);
      } else if (command === "settings") openSettings("general");
      else if (command === "usage") openOnHost(host.id, { kind: "usage" });
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  // Shown in the main pane's top row only while the sidebar is hidden.
  const showSidebar = !sidebarOpen && (
    <IconButton
      label="Show sidebar"
      command="sidebar"
      aria-expanded={false}
      aria-controls="sidebar"
      onClick={() => setSidebarOpen(true)}
    >
      <PanelLeftOpen />
    </IconButton>
  );
  // The side panel is a chat's, so Settings and Usage have none.
  const chat = !settings && selection.kind !== "usage";
  const sidePanelOpen = panelOpen && chat;
  const expanded = sidePanelOpen && panelExpanded;
  // The main pane's top row meets the traffic lights without the sidebar, and
  // Windows' window buttons without the side panel.
  const topBarInset = [
    sidebarOpen ? "" : "traffic-light-inset",
    sidePanelOpen ? "" : "window-controls-inset",
  ].join(" ");

  return (
    <div className="flex h-full">
      {hosts.map((h) => (
        <HostLoader key={h.id} hostId={h.id} onView={report} />
      ))}
      <NewThreadPicker
        ref={picker}
        groups={groups}
        repos={threads.state.repos}
        hostName={host.name}
        onPick={newThread}
      />
      <Sidebar
        open={sidebarOpen}
        onClose={() => setSidebarOpen(false)}
        onNewThread={() => newThread()}
      >
        {settings ? (
          <SettingsNav
            section={settings}
            onSection={(section) => openSettings(section)}
            onBack={() => setSettings(null)}
          />
        ) : (
          <ThreadList
            hosts={listed}
            host={host}
            selection={selection}
            onSelect={openOnHost}
            onOpenProject={openProject}
            onOpenSettings={openSettings}
            onDelete={deleteThread}
            onNewThread={() => newThread()}
          />
        )}
      </Sidebar>

      {/* An expanded side panel takes the main pane's place. */}
      <main hidden={expanded} className="flex min-w-0 flex-1 flex-col bg-background">
        {settings ? (
          <>
            <TopBar className={topBarInset}>
              {showSidebar}
              <Breadcrumb items={[{ label: "Settings" }, { label: settingsNames[settings] }]} />
            </TopBar>
            <Settings
              section={settings}
              listed={listed}
              theme={theme}
              onThemeChange={setTheme}
              sourceControlHost={settingsHost}
            />
          </>
        ) : selection.kind === "usage" ? (
          <UsagePage hosts={hosts} leading={showSidebar} topBarClassName={topBarInset} />
        ) : (
          <>
            <TopBar className={`@container ${topBarInset}`}>
              {showSidebar}
              <Breadcrumb items={crumbs} />
              <div className="ml-auto flex shrink-0 items-center gap-2">
                {selection.kind !== "project" && group.id !== noRepo && (
                  <Actions hostId={host.id} repoId={group.id} canRun={!!folder} onRun={runAction} />
                )}
                <OpenMenu
                  hostId={host.id}
                  // The thread's worktree, or New thread's repository.
                  folder={
                    selection.kind === "thread"
                      ? threads.state.runs[selection.threadId]?.worktreePath
                      : selection.kind === "new"
                        ? threads.state.repos.find((r) => r.id === group.id)?.path
                        : undefined
                  }
                />
                {selection.kind === "thread" && (
                  <GitMenu
                    key={`${host.id}/${selection.threadId}`}
                    hostId={host.id}
                    run={threads.state.runs[selection.threadId]}
                    onPrOpened={linksPrs ? openPr : undefined}
                    onSetUpGithub={setUpGithub}
                  />
                )}
                {folder && (
                  <IconButton
                    label={drawerOpen ? "Hide terminal" : "Show terminal"}
                    command="terminal"
                    aria-pressed={drawerOpen}
                    aria-controls="terminal-drawer"
                    onClick={toggleDrawer}
                  >
                    <PanelBottom />
                  </IconButton>
                )}
                {/* Shown only while the panel is closed; the panel's top bar has it otherwise. */}
                {!panelOpen && (
                  <IconButton
                    label="Show side panel"
                    command="panel"
                    aria-expanded={false}
                    aria-controls="side-panel"
                    onClick={() => setPanelOpen(true)}
                  >
                    <PanelRight />
                  </IconButton>
                )}
              </div>
            </TopBar>
            {selection.kind === "thread" ? (
              // Keyed, so another run starts from an empty transcript.
              <AgentChat
                key={`${host.id}/${selection.threadId}`}
                hostId={host.id}
                runId={selection.threadId}
                notice={notice?.threadId === selection.threadId ? notice.text : undefined}
                prompt={threads.state.runs[selection.threadId]?.prompt}
                // The list's status goes stale once the run moves on, so only a start says so.
                going={selection.started}
                noRepo={group.id === noRepo}
                pullRequests={prs.urls.length > 0 && <PullRequestChip prs={prs} onOpen={openPr} />}
                onPrOpened={linksPrs ? openPr : undefined}
                onSetUpGithub={setUpGithub}
                compose={compose}
                onComposed={composed}
              />
            ) : selection.kind === "new" ? (
              <NewThread
                key={host.id}
                hostId={host.id}
                hosts={hosts}
                groups={groups}
                groupId={group.id}
                onGroupChange={(groupId) => setSelection({ kind: "new", groupId })}
                local={host.id === localId}
                addRepo={threads.addRepo}
                start={threads.start}
                runOptions={
                  connection?.status === "connected" && "runOptions" in connection.capabilities
                }
                onStarted={(threadId, text, background) => {
                  setNotice(text ? { threadId, text } : undefined);
                  if (!background) setSelection({ kind: "thread", threadId, started: true });
                }}
                disabledReason={offline}
              />
            ) : agentId ? (
              <AgentChat
                key={`${host.id}/${agentId}`}
                hostId={host.id}
                runId={agentId}
                prompt={agent?.prompt}
                // A Project's subagents are kept current.
                going={isRunning(agent?.status)}
                others={othersAsked(agentId)}
              />
            ) : (
              project && (
                <ProjectChat
                  key={`${host.id}/${project.id}`}
                  hostId={host.id}
                  project={project}
                  prompt={project.coordinator && threads.state.runs[project.coordinator]?.prompt}
                  startCoordinator={threads.startCoordinator}
                  others={othersAsked(project.coordinator)}
                />
              )
            )}
          </>
        )}
        {/* Outside the views, so the terminals live on behind Settings and Usage. */}
        <TerminalDrawer
          open={drawerOpen}
          folder={folder}
          deleted={deleted}
          onClose={toggleDrawer}
        />
      </main>

      <SidePanel
        open={sidePanelOpen}
        onClose={() => setPanelOpen(false)}
        expanded={expanded}
        onExpandedChange={setPanelExpanded}
        leading={expanded && showSidebar}
        topBarClassName={expanded && !sidebarOpen ? "traffic-light-inset" : ""}
        remoteHost={host.id === localId ? undefined : host.name}
        browse={browse}
        pullRequest={showPr}
        pullRequests={
          linksPrs && threadRun
            ? {
                urls: prs.urls,
                list: <PullRequestList prs={prs} onOpen={openPr} />,
                view: (url) => (
                  <PullRequestView
                    key={`${host.id}/${threadRun.id}`}
                    url={url}
                    prs={prs}
                    onSetUpGithub={setUpGithub}
                    onCompose={(text, send) => {
                      // The chat is under an expanded panel.
                      setPanelExpanded(false);
                      setCompose({ text, send });
                    }}
                  />
                ),
              }
            : undefined
        }
        agents={
          project && (
            <AgentsPanel
              // Another Project's start box starts empty, with its own retry id.
              key={`${host.id}/${project.id}`}
              agents={agents}
              openId={agentId}
              onOpen={openAgent}
              disabledReason={offline}
            />
          )
        }
        files={
          filesRunId && (
            <FilesPanel
              key={`${host.id}/${filesRunId}`}
              hostId={host.id}
              runId={filesRunId}
              unavailable={
                offline ??
                (connected && "files" in connection.capabilities
                  ? undefined
                  : "Update Parallax on this host to browse a thread's files.")
              }
            />
          )
        }
        context={
          project && (
            <ContextPanel
              key={`${host.id}/${project.id}`}
              hostId={host.id}
              project={project.id}
              name={project.name}
              connected={connected}
            />
          )
        }
        terminal={(shown, empty) => (
          <TerminalPool
            prefix="panel"
            label="Side panel terminal"
            active={shown ? folder : undefined}
            deleted={deleted}
            empty={empty}
          />
        )}
      />
    </div>
  );
}

/** Loads one host's threads and Projects and hands them up, whenever they change. */
function HostLoader({
  hostId,
  onView,
}: {
  hostId: string;
  onView: (hostId: string, view: ThreadsView) => void;
}) {
  const connection = useConnection(hostId);
  const capabilities = connection?.status === "connected" ? connection.capabilities : undefined;
  const view = useThreads(hostId, !!capabilities, {
    approvals: !!capabilities && "approvals" in capabilities,
    attention: !!capabilities && "threadAttention" in capabilities,
    editable: !!capabilities && "projectEdit" in capabilities,
    deletable: !!capabilities && "projectDelete" in capabilities,
    iconImageBytes: iconImageBytes(connection),
  });
  useEffect(() => onView(hostId, view), [hostId, view, onView]);
  return null;
}
