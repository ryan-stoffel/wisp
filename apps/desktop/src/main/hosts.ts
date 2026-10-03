import { app, BrowserWindow, ipcMain, powerMonitor, type WebContents } from "electron";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { stat } from "node:fs/promises";
import { homedir, hostname } from "node:os";
import path from "node:path";

import { ErrorCodes, type CliKind } from "../protocol/generated/protocol";
import {
  type ConnectionState,
  type RendererMethod,
  type RpcResponse,
  type SshHost,
  type SubscribeParams,
} from "../preload/bridge";
import { Connection, sshCommand } from "./connection";
import { checkHost, readSettings, writeSettings, type Settings } from "./settings";
import {
  closeAllTerminals,
  closeTerminal,
  isCliKind,
  loginCommand,
  openTerminal,
  resizeTerminal,
  shellCommand,
  writeTerminal,
  type Command,
} from "./terminal";
import { dataDir, findPlxd, replaceServe, plxdVersion } from "./plxd";

// The methods the renderer may call, checked at runtime because the renderer is untrusted
// (0022). Typed so that adding a method to the protocol fails the type-check until it is here.
const rendererMethods: Record<RendererMethod, true> = {
  "host/health": true,
  "host/version": true,
  "project/list": true,
  "project/create": true,
  "project/start": true,
  "project/update": true,
  "project/delete": true,
  "accounts/keys/add": true,
  "accounts/keys/list": true,
  "accounts/keys/remove": true,
  "accounts/list": true,
  "accounts/refresh": true,
  "usage/get": true,
  "usage/history": true,
  "usage/daily": true,
  "accounts/defaults/get": true,
  "accounts/defaults/set": true,
  "context/list": true,
  "context/read": true,
  "context/write": true,
  "agent/start": true,
  "agent/send": true,
  "agent/cancel": true,
  "agent/list": true,
  "agent/events": true,
  "agent/image": true,
  "agent/diff": true,
  "agent/file": true,
  "agent/files": true,
  "agent/accept": true,
  "agent/requestChanges": true,
  "agent/openPr": true,
  "agent/gitStatus": true,
  "agent/commit": true,
  "agent/push": true,
  "agent/approve": true,
  "thread/list": true,
  "repo/add": true,
  "thread/start": true,
  "thread/archive": true,
  "thread/delete": true,
  "thread/update": true,
  "thread/search": true,
  "repo/update": true,
  "repo/refs": true,
  "pr/view": true,
  "pr/act": true,
  "pr/diff": true,
  "pr/link": true,
  "pr/unlink": true,
  "agent/commands": true,
  "repo/files": true,
  "github/status": true,
  "agent/resumeNow": true,
  "agent/autoResume": true,
  "host/settings/get": true,
  "host/settings/set": true,
};

/** Every host's connection, by host id: `local`, then each saved SSH host. */
const connections = new Map<string, Connection>();
/** Each window's subscriptions, by the key its preload chose. */
const subscriptions = new Map<WebContents, Map<string, () => void>>();

let settings: Settings = { hosts: [] };
/** Why `settings.json` couldn't be read, which blocks saving over it. */
let settingsError: string | undefined;
const settingsFile = () => path.join(app.getPath("userData"), "settings.json");

/**
 * Connects to the local plxd, as host id `local`, and to every saved SSH host at once, and
 * serves the `window.parallax` calls that reach plxd or edit the hosts.
 */
export function startHosts(): void {
  addConnection("local", () => {
    const plxd = localPlxd();
    return plxd === undefined ? undefined : [plxd, "attach"];
  });
  try {
    settings = readSettings(settingsFile());
  } catch (error) {
    settingsError = `Parallax can't use ${settingsFile()}, so it won't save over it: ${(error as Error).message.replace(/\.$/, "")}. Fix or remove the file, then restart Parallax.`;
    console.error(settingsError);
  }
  for (const host of settings.hosts) addSshConnection(host);

  ipcMain.handle("parallax:hosts", () => settings.hosts);
  ipcMain.handle("parallax:saveHost", (_event, input: unknown, id: unknown) => saveHost(input, id));
  ipcMain.handle("parallax:removeHost", (_event, id: unknown) => removeHost(id));
  ipcMain.handle("parallax:localName", () => localName());
  ipcMain.handle("parallax:renameLocal", (_event, name: unknown) => {
    if (typeof name !== "string") return "invalid name";
    const label = name
      .replace(/\p{Cc}/gu, "")
      .trim()
      .slice(0, 64);
    const { localName: _old, ...rest } = settings;
    const error = saveSettings(label ? { ...rest, localName: label } : rest);
    if (!error) broadcast("parallax:localName", localName());
    return error;
  });

  // Answers `{error}` rather than throwing, so a bad call reads like any failed request.
  ipcMain.handle(
    "parallax:request",
    (_event, hostId: unknown, method: unknown, params: unknown) => {
      if (typeof method !== "string" || !Object.hasOwn(rendererMethods, method)) {
        return invalid(ErrorCodes.MethodNotFound, `unknown method: ${String(method)}`);
      }
      const host = typeof hostId === "string" ? connections.get(hostId) : undefined;
      if (!host) return invalid(ErrorCodes.InvalidParams, `unknown host: ${String(hostId)}`);
      // plxd validates the params' shape; they only have to be an object to be sent.
      if (!isObject(params)) return invalid(ErrorCodes.InvalidParams, "params must be an object");
      return host.request(method as RendererMethod, params as never);
    },
  );

  ipcMain.handle("parallax:subscribe", (event, hostId: unknown, key: unknown, params: unknown) => {
    if (!isObject(params)) throw new Error("params must be an object");
    const { after, project, logId } = params;
    if (typeof key !== "string") throw new Error("invalid subscription key");
    if (typeof logId !== "string") throw new Error("logId must be a string");
    if (typeof after !== "number" || !Number.isSafeInteger(after) || after < 0) {
      throw new Error("after must be a non-negative integer");
    }
    if (project !== undefined && typeof project !== "string") throw new Error("invalid project");
    const host = connection(hostId);
    const sender = event.sender;
    // A reused key replaces its subscription instead of leaking the old one.
    windowSubscriptions(sender).get(key)?.();
    let ended = false;
    const unsubscribe = host.subscribe(
      { after, logId, ...(project !== undefined && { project }) } satisfies SubscribeParams,
      (message) => {
        if (message.type !== "event") {
          ended = true;
          windowSubscriptions(sender).delete(key);
        }
        if (!sender.isDestroyed()) sender.send("parallax:subscription", key, message);
      },
    );
    // It ends at once when `logId` is stale.
    if (!ended) windowSubscriptions(sender).set(key, unsubscribe);
  });

  ipcMain.handle("parallax:unsubscribe", (event, key: unknown) => {
    const unsubscribe = subscriptions.get(event.sender)?.get(String(key));
    subscriptions.get(event.sender)?.delete(String(key));
    unsubscribe?.();
  });

  ipcMain.handle("parallax:connectionState", (_event, hostId: unknown) => connection(hostId).state);
  ipcMain.handle("parallax:retry", (_event, hostId: unknown) => connection(hostId).retry());

  // A window's terminals (terminal.ts), by an id it picks: a CLI's sign-in, or a shell in a
  // thread's folder. The renderer names the host and the CLI or folder; only main decides what runs.
  ipcMain.handle(
    "parallax:openTerminal",
    (event, id: unknown, target: unknown, cols: unknown, rows: unknown) => {
      if (!isTerminalId(id) || !isObject(target) || !isSize(cols) || !isSize(rows)) {
        return "invalid terminal";
      }
      const { hostId, cli, path } = target;
      if (typeof hostId !== "string") return "invalid terminal";
      if (isCliKind(cli)) {
        return openTerminal(event.sender, id, () => signInCommand(hostId, cli), cols, rows);
      }
      if (typeof path !== "string") return "invalid terminal";
      return openTerminal(event.sender, id, () => folderCommand(hostId, path), cols, rows);
    },
  );
  ipcMain.on("parallax:terminalInput", (event, id: unknown, data: unknown) => {
    if (isTerminalId(id) && typeof data === "string") writeTerminal(event.sender, id, data);
  });
  ipcMain.on("parallax:resizeTerminal", (event, id: unknown, cols: unknown, rows: unknown) => {
    if (isTerminalId(id) && isSize(cols) && isSize(rows))
      resizeTerminal(event.sender, id, cols, rows);
  });
  ipcMain.on("parallax:closeTerminal", (event, id: unknown) => {
    if (isTerminalId(id)) closeTerminal(event.sender, id);
  });

  powerMonitor.on("resume", () => {
    for (const each of connections.values()) each.heartbeat();
  });
  app.on("will-quit", () => {
    for (const each of connections.values()) each.dispose();
    closeAllTerminals();
  });
}

/**
 * What signs in to `cli` on a host: the binary that host's plxd found, which is the one it runs
 * later, reached as the host's connection is. Resolves to an error for people.
 */
async function signInCommand(hostId: string, cli: CliKind): Promise<Command | string> {
  const host = connections.get(hostId);
  if (!host) return "That host isn't in Parallax anymore.";
  // Decided before asking, so a remote host's path can never run on this computer.
  const saved = settings.hosts.find((h) => h.id === hostId);
  const ssh = saved && { destination: saved.destination, ssh: settings.ssh ?? "ssh" };
  const answer = await host.request("accounts/list", {});
  if ("error" in answer)
    return `Parallax couldn't ask the host where the CLI is: ${answer.error.message}`;
  const path = answer.result.clis.find((each) => each.cli === cli)?.path;
  if (!path) return "That CLI isn't installed on this host anymore.";
  return loginCommand(cli, path, ssh);
}

/**
 * This computer's name in Parallax: the user's, else the computer's own. macOS's is the Computer
 * Name in System Settings, such as "macbook"; elsewhere, the host name.
 */
function localName(): string {
  return settings.localName || (computerName ??= readComputerName());
}
let computerName: string | undefined;
function readComputerName(): string {
  if (process.platform === "darwin") {
    try {
      const name = execFileSync("scutil", ["--get", "ComputerName"], { encoding: "utf8" }).trim();
      if (name) return name;
    } catch {
      // Falls back to the host name.
    }
  }
  return hostname().replace(/\.local$/, "");
}

/** A saved SSH host by id. Undefined for this computer, `local`, and for an unknown id. */
export const savedHost = (id: string): SshHost | undefined =>
  settings.hosts.find((h) => h.id === id);

/**
 * What opens a shell in `folder` on a host: here, if it's still a folder; on an SSH host, over
 * ssh as the host's connection is. Resolves to an error for people.
 */
async function folderCommand(hostId: string, folder: string): Promise<Command | string> {
  if (!connections.has(hostId)) return "That host isn't in Parallax anymore.";
  const saved = settings.hosts.find((h) => h.id === hostId);
  // A Windows path, which can't hold a `"`, goes to the host in double quotes.
  if (saved && /^[a-z]:\\/i.test(folder) && folder.includes('"'))
    return `${folder} isn't a folder.`;
  if (saved)
    return shellCommand(folder, { destination: saved.destination, ssh: settings.ssh ?? "ssh" });
  const isFolder =
    path.isAbsolute(folder) && (await stat(folder).catch(() => undefined))?.isDirectory();
  return isFolder ? shellCommand(folder) : `${folder} isn't a folder on this computer anymore.`;
}

/** The local `plxd` binary (plxd.ts). */
const localPlxd = () =>
  findPlxd({
    env: process.env,
    platform: process.platform,
    packaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    appPath: app.getAppPath(),
  });

/**
 * How often this launch has compared the local `plxd serve`'s version with its plxd's: at most
 * twice, so a re-attach to a serve still shutting down gets one more try.
 */
let serveChecks = 0;

/**
 * After an update, the local `plxd serve` may still be the previous app's, since attach reuses a
 * running one (0010). A packaged app replaces it when its version differs from the bundled
 * plxd's, older or newer (a move back to Standard), and reconnects, so attach starts the bundled
 * one. Only a serve a packaged Parallax started is replaced (`replaceServe`), never a dev checkout's or
 * a plxd on PATH. Not on Windows, whose installer stops every process running from the app's
 * folder, `plxd.exe` included.
 */
async function replaceOtherServe(state: ConnectionState): Promise<void> {
  if (!app.isPackaged || serveChecks >= 2 || process.platform === "win32") return;
  const running =
    state.status === "connected"
      ? state.plxd
      : state.status === "failed"
        ? state.error.plxd
        : undefined;
  if (running === undefined) return;
  serveChecks++;
  const plxd = localPlxd();
  const bundled = plxd === undefined ? undefined : await plxdVersion(plxd);
  const { stopped, why } = await replaceServe(
    dataDir(process.env, process.platform, homedir()),
    running,
    bundled,
  );
  console.log(`parallax: ${why}`);
  if (stopped) connections.get("local")?.retry();
}

/** Starts a host's connection, replacing any it had. */
function addConnection(hostId: string, command: () => string[] | undefined, destination?: string) {
  connections.get(hostId)?.dispose();
  const created = new Connection({
    command,
    ...(destination !== undefined && { destination }),
    clientVersion: app.getVersion(),
    onState: (state) => {
      // A replaced or removed connection has nothing more to say.
      if (connections.get(hostId) !== created) return;
      broadcast("parallax:state", hostId, state);
      if (hostId === "local") void replaceOtherServe(state);
    },
  });
  connections.set(hostId, created);
  created.start();
}

function addSshConnection({ id, destination }: SshHost): void {
  addConnection(id, () => sshCommand(destination, settings.ssh), destination);
}

/** `window.parallax.saveHost`. Its input comes from the renderer, so it's checked here. */
function saveHost(input: unknown, id: unknown): string | undefined {
  if (!isObject(input) || typeof input["name"] !== "string") return "invalid host";
  if (typeof input["destination"] !== "string") return "invalid host";
  const checked = checkHost({ name: input["name"], destination: input["destination"] });
  if (typeof checked === "string") return checked;
  const old = settings.hosts.find((h) => h.id === id);
  if (id !== undefined && !old) return "That host isn't in Parallax anymore.";
  const host: SshHost = { id: old?.id ?? randomUUID(), ...checked };
  const hosts = old ? settings.hosts.map((h) => (h === old ? host : h)) : [...settings.hosts, host];
  const error = saveSettings({ ...settings, hosts });
  if (error) return error;
  // A rename keeps the connection; a new destination needs a new one.
  if (old?.destination !== host.destination) addSshConnection(host);
  return undefined;
}

/** `window.parallax.removeHost`. Resolves to an error for people, as `saveHost` does. */
function removeHost(id: unknown): string | undefined {
  if (typeof id !== "string" || !settings.hosts.some((h) => h.id === id)) return undefined;
  const error = saveSettings({ ...settings, hosts: settings.hosts.filter((h) => h.id !== id) });
  if (error) return error;
  connections.get(id)?.dispose();
  connections.delete(id);
  return undefined;
}

/** Writes the settings and tells every window about the hosts. Resolves to an error for people. */
function saveSettings(next: Settings): string | undefined {
  if (settingsError) return settingsError;
  try {
    writeSettings(settingsFile(), next);
  } catch (error) {
    return `Parallax couldn't save its settings: ${(error as Error).message}`;
  }
  settings = next;
  broadcast("parallax:hosts", next.hosts);
  return undefined;
}

function connection(hostId: unknown): Connection {
  const found = typeof hostId === "string" ? connections.get(hostId) : undefined;
  if (!found) throw new Error(`unknown host: ${String(hostId)}`);
  return found;
}

/** A terminal's id, which its window picks. */
const isTerminalId = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0 && value.length <= 500;

/** A terminal's width or height, in character cells. */
const isSize = (value: unknown): value is number =>
  Number.isInteger(value) && (value as number) > 0 && (value as number) <= 1000;

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function invalid(code: number, message: string): RpcResponse<never> {
  return { error: { code, message } };
}

function broadcast(channel: string, ...args: unknown[]): void {
  for (const window of BrowserWindow.getAllWindows()) window.webContents.send(channel, ...args);
}

/** A window's subscriptions, ended when it closes or reloads, since its listeners are gone. */
function windowSubscriptions(sender: WebContents): Map<string, () => void> {
  let own = subscriptions.get(sender);
  if (!own) {
    const created = new Map<string, () => void>();
    const endAll = () => {
      for (const unsubscribe of created.values()) unsubscribe();
      created.clear();
    };
    // `did-navigate` fires when a main-frame navigation commits, such as a reload. Not
    // `did-start-navigation`, which also fires for link clicks that `will-navigate` cancels.
    sender.on("did-navigate", endAll);
    sender.once("destroyed", () => {
      endAll();
      subscriptions.delete(sender);
    });
    subscriptions.set(sender, created);
    own = created;
  }
  return own;
}
