// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";

import type { ConnectionState, Profile, SshHost, ParallaxBridge } from "../preload/bridge";
import type { SettingsSection } from "./App";
import { models } from "./models";
import { Settings } from "./Settings";
import { appShortcut } from "./ui";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

// xterm.js needs a real canvas; the stand-in only marks where the terminal is.
vi.mock("./SignInTerminal", () => ({
  SignInTerminal: ({ name }: { name: string }) => (
    <div role="group" aria-label={`${name} sign-in terminal`} />
  ),
}));

const mini: SshHost = { id: "h-mini", name: "Mac mini", destination: "mini" };
const states: Record<string, ConnectionState> = {
  local: { status: "connected", plxd: "0.1.0", protocol: 1, capabilities: {} },
  [mini.id]: {
    status: "failed",
    retrying: false,
    error: { reason: "exited", message: "plxd exited." },
  },
};
const secret = "sk-proj-THE-SECRET-0123456789abcdef";

type Answer = { result: unknown } | { error: { code: number; message: string; data?: object } };
let answers: Record<string, (params: Record<string, unknown>) => Answer | Promise<Answer>>;
const request = vi.fn(async (_host: string, method: string, params: Record<string, unknown>) => {
  const answer = (await answers[method]?.(params)) ?? { error: { code: -32601, message: "no" } };
  return "result" in answer ? { ...answer, logId: "log" } : answer;
});
const calls = (method: string) =>
  request.mock.calls.filter(([, m]) => m === method).map(([host, , params]) => ({ host, params }));

beforeEach(() => {
  request.mockClear();
  answers = {
    "accounts/list": () => ({
      result: {
        checkedAt: "2026-09-28T12:00:00Z",
        clis: [
          { cli: "claude", installed: true, version: "2.1.281", plan: "max", signedIn: true },
          { cli: "codex", installed: true, version: "0.156.1", signedIn: false },
          { cli: "cursor", installed: false },
        ],
      },
    }),
    "accounts/keys/list": () => ({
      result: {
        accounts: [
          {
            id: "k-work",
            provider: "anthropic",
            label: "Work",
            createdAt: "2026-09-28T12:00:00Z",
            maskedKey: "sk-ant-...abcd",
          },
        ],
      },
    }),
  };
  window.parallax = {
    platform: "darwin",
    connectionState: async (hostId) => states[hostId]!,
    onConnectionState: () => () => {},
    hosts: async () => [mini],
    onHosts: () => () => {},
    onLocalName: (listener: (name: string) => void) => {
      listener("This Mac");
      return () => {};
    },
    setZoom: () => {},
    setAppIcon: () => {},
    request: request as unknown as ParallaxBridge["request"],
  } as Partial<ParallaxBridge> as ParallaxBridge;
});

let unmount = () => {};
afterEach(() => {
  act(() => unmount());
  vi.useRealTimers();
});

async function renderSettings(name: SettingsSection = "providers") {
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  act(() =>
    root.render(<Settings section={name} listed={[]} theme="system" onThemeChange={() => {}} />),
  );
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
  };
  await settle();
}

const settle = async () => {
  for (let i = 0; i < 10; i++) await act(async () => {});
};
// The first match outside a hidden tab panel.
const visible = (selector: string) =>
  [...document.querySelectorAll<HTMLElement>(selector)].find((e) => !e.closest("[hidden]"))!;
const section = (name: string) => visible(`[aria-label="${name}"]`);
const rows = (name: string) =>
  [...section(name).querySelectorAll(":scope > div:last-child > div")].map((r) => r.textContent);
const button = (within: Element, name: string) =>
  [...within.querySelectorAll("button")].find((b) => b.textContent === name)!;
const tabs = () => [...document.querySelectorAll('[role="tab"]')].map((t) => t.textContent);
const tab = (name: string) =>
  [...document.querySelectorAll<HTMLElement>('[role="tab"]')].find((t) =>
    t.textContent?.startsWith(name),
  )!;
const pane = () => visible('[role="tabpanel"]');
// The Work key's row.
const work = () =>
  [...section("Anthropic API keys").querySelectorAll("div")].find((d) =>
    d.textContent?.startsWith("Work"),
  )!;
const click = async (element: HTMLElement) => {
  await act(async () => element.click());
  await settle();
};
const change = (element: HTMLSelectElement | HTMLInputElement, value: string) =>
  act(() => {
    Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value")!.set!.call(
      element,
      value,
    );
    element.dispatchEvent(
      new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }),
    );
  });

test("lists the host's CLIs, and shows the chosen one's account, API keys, and models", async () => {
  await renderSettings();
  expect(tabs()).toEqual([
    "Claude Code2.1.281Signed in · Max",
    "Codex0.156.1Not signed in",
    "CursorNot installed",
  ]);
  expect(tab("Claude Code").getAttribute("aria-selected")).toBe("true");
  expect(pane().getAttribute("aria-labelledby")).toBe(tab("Claude Code").id);
  expect(rows("Account")).toEqual(["Signed in · Max"]);
  expect(rows("Anthropic API keys")).toEqual(["WorkAnthropic API key · sk-ant-...abcdRemove"]);
  expect(rows("Models")).toEqual(
    models.filter((m) => m.provider === "Claude").map((m) => m.name + m.id),
  );

  // Down moves to the next tab and chooses it.
  await act(async () =>
    tab("Claude Code").dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
    ),
  );
  expect(document.activeElement).toBe(tab("Codex"));
  expect(rows("Account")).toEqual(["Not signed inSign in"]);
  expect(rows("OpenAI API keys")).toEqual([]);

  // Cursor takes no API key (0004), and links to its install page.
  await click(tab("Cursor"));
  expect(rows("Account")).toEqual([
    "Not installedInstall Cursor on this host, then refresh.Install",
  ]);
  expect(pane().querySelector('[aria-label$="API keys"]')).toBeNull();
  const install = section("Account").querySelector("a")!;
  expect(install.href).toBe("https://cursor.com/docs/cli/installation");
  expect(install.target).toBe("_blank");
  expect(request.mock.calls.every(([host]) => host === "local")).toBe(true);
});

test("a sign-in stays open while another tab is chosen", async () => {
  await renderSettings();
  await click(tab("Codex"));
  await click(button(pane(), "Sign in"));
  const terminal = section("Codex sign-in terminal");
  expect(terminal).toBeDefined();

  await click(tab("Claude Code"));
  expect(terminal.closest("[hidden]")).not.toBeNull();
  await click(tab("Codex"));
  expect(section("Codex sign-in terminal")).toBe(terminal);
});

test("opening a sign-in closes the other one, since the window runs one terminal", async () => {
  answers["accounts/list"] = () => ({
    result: {
      checkedAt: "2026-09-28T12:00:00Z",
      clis: [
        { cli: "claude", installed: true, signedIn: false },
        { cli: "codex", installed: true, signedIn: false },
      ],
    },
  });
  await renderSettings();
  await click(button(pane(), "Sign in"));
  await click(tab("Codex"));
  await click(button(pane(), "Sign in"));
  const terminals = [...document.querySelectorAll('[aria-label$="sign-in terminal"]')];
  expect(terminals.map((t) => t.getAttribute("aria-label"))).toEqual(["Codex sign-in terminal"]);
});

test("keys still show when the CLIs can't be checked", async () => {
  answers["accounts/list"] = () => ({ error: { code: -32000, message: "probe failed" } });
  await renderSettings();
  expect(tabs()).toEqual([]);
  expect(rows("API keys")).toEqual(["WorkAnthropic API key · sk-ant-...abcdRemove"]);
});

test("the host picker shows another host's state", async () => {
  await renderSettings();
  const picker = section("Host") as HTMLSelectElement;
  await act(async () => {
    picker.value = mini.id;
    picker.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle();
  expect(tabs()).toEqual([]);
  expect(document.body.textContent).toContain("Disconnected");
});

test("adds a key, clearing it from its field after each try, and never gets it back", async () => {
  await renderSettings();
  await click(tab("Codex"));
  await click(button(section("OpenAI API keys"), "Add API key"));
  const form = document.querySelector<HTMLFormElement>('form[aria-label="Add API key"]')!;
  const keyField = form.querySelector<HTMLInputElement>('[name="key"]')!;
  const submit = async () => {
    form.querySelector<HTMLInputElement>('[name="label"]')!.value = "Personal";
    keyField.value = secret;
    await act(async () => form.requestSubmit());
    await settle();
  };

  answers["accounts/keys/add"] = () => ({
    error: {
      code: -32000,
      message: "the keychain is locked or access was denied",
      data: { kind: "keychainUnavailable" },
    },
  });
  await submit();
  expect(form.querySelector('[role="alert"]')!.textContent).toBe(
    "This host's keychain isn't available: the keychain is locked or access was denied.",
  );
  expect(keyField.value).toBe("");

  answers["accounts/keys/add"] = (p) => ({
    result: {
      account: {
        id: p["id"],
        provider: p["provider"],
        label: p["label"],
        createdAt: "2026-09-28T12:05:00Z",
        maskedKey: "sk-proj-...cdef",
      },
    },
  });
  await submit();
  const [first, retry] = calls("accounts/keys/add");
  expect(retry).toEqual(first);
  expect(first!.params).toMatchObject({ provider: "openai", label: "Personal", key: secret });
  expect(document.querySelector("form")).toBeNull();
  expect(rows("OpenAI API keys")).toEqual(["PersonalOpenAI API key · sk-proj-...cdefRemove"]);
  expect(document.body.innerHTML).not.toContain("THE-SECRET");
});

test("removes a key only once it's confirmed", async () => {
  answers["accounts/keys/remove"] = () => ({ result: {} });
  await renderSettings();
  await click(button(work(), "Remove"));
  expect(work().textContent).toContain("Remove this key?");
  await click(button(work(), "Cancel"));
  expect(calls("accounts/keys/remove")).toEqual([]);

  await click(button(work(), "Remove"));
  await click(button(work(), "Remove"));
  expect(calls("accounts/keys/remove")).toEqual([{ host: "local", params: { id: "k-work" } }]);
  expect(rows("Anthropic API keys")).toEqual([]);
});

test("a key another client already removed goes when removed", async () => {
  answers["accounts/keys/remove"] = () => ({
    error: { code: -32000, message: "account not found", data: { kind: "accountNotFound" } },
  });
  await renderSettings();
  await click(button(work(), "Remove"));
  await click(button(work(), "Remove"));
  expect(rows("Anthropic API keys")).toEqual([]);
  expect(document.querySelector('[role="alert"]')).toBeNull();
});

test("Refresh probes the CLIs again, without undoing a remove made meanwhile", async () => {
  let probed = () => {};
  answers["accounts/refresh"] = () =>
    new Promise((resolve) => {
      probed = () =>
        resolve({
          result: {
            checkedAt: "2026-09-28T12:10:00Z",
            clis: [{ cli: "claude", installed: true, signedIn: false }],
          },
        });
    });
  answers["accounts/keys/remove"] = () => ({ result: {} });
  await renderSettings();
  await click(document.querySelector<HTMLElement>('[aria-label="Refresh"]')!);
  await click(button(work(), "Remove"));
  await click(button(work(), "Remove"));
  await act(async () => probed());
  await settle();
  expect(calls("accounts/refresh")).toHaveLength(1);
  expect(tabs()).toEqual(["Claude CodeNot signed in"]);
  expect(rows("Anthropic API keys")).toEqual([]);
});

test("shows each account's usage and limits for the chosen period, and keeps them live", async () => {
  vi.useFakeTimers({ now: Date.parse("2026-09-28T12:00:00Z"), toFake: ["setTimeout", "Date"] });
  const tokens = (input: number, output = 0) => ({
    inputTokens: input,
    outputTokens: output,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
  });
  let used = 12;
  answers["usage/get"] = () => ({
    result: {
      accounts: [
        {
          accountId: "claude",
          today: { ...tokens(1000, 200), costUsdMicros: 500_000 },
          week: { ...tokens(40_000, 2000), costUsdMicros: 900_000 },
          limits: [
            {
              window: "five_hour",
              usedPercent: used,
              resetsAt: "2026-09-28T14:00:00Z",
              capturedAt: "2026-09-28T11:59:00Z",
            },
          ],
        },
        { accountId: "k-work", today: tokens(0), week: tokens(3_000_000), limits: [] },
        { accountId: "k-gone", today: tokens(5), week: tokens(5), limits: [] },
      ],
    },
  });
  await renderSettings();
  expect(document.body.textContent).toContain("Checked just now");
  expect(rows("Usage")).toEqual([
    "1.2K tokens today, about $0.505-hour limit · 12% used · resets in 2 h",
  ]);
  expect(rows("Anthropic API keys")).toEqual([
    "WorkAnthropic API key · sk-ant-...abcdNo usage todayRemove",
  ]);

  await click(section("Usage period").querySelector<HTMLInputElement>('[value="week"]')!);
  expect(rows("Usage")).toEqual([
    "42K tokens this week, about $0.905-hour limit · 12% used · resets in 2 h",
  ]);
  expect(rows("Anthropic API keys")).toEqual([
    "WorkAnthropic API key · sk-ant-...abcd3M tokens this weekRemove",
  ]);
  await click(tab("Codex"));
  expect(rows("Usage")).toEqual(["No usage this week"]);

  // A run hits the limit: the next poll shows it.
  used = 100;
  await click(tab("Claude Code"));
  await act(() => vi.advanceTimersByTimeAsync(5000));
  await settle();
  expect(calls("usage/get")).toHaveLength(2);
  expect(rows("Usage")[0]).toContain("5-hour limit reached · resets in 2 h");
});

test("a provider's switch turns it off on this computer, and back on", async () => {
  await renderSettings();
  const toggle = () =>
    document.querySelector<HTMLButtonElement>('[role="switch"][aria-label="Use Codex"]')!;
  expect(toggle().getAttribute("aria-checked")).toBe("true");
  await click(toggle());
  expect(tabs()[1]).toBe("Codex0.156.1Off");
  expect(JSON.parse(localStorage.getItem("parallax.disabledProviders")!)).toEqual(["codex"]);
  await click(toggle());
  expect(tabs()[1]).toBe("Codex0.156.1Not signed in");
});

test("a shortcut can be added, refused when another command has it, removed, and reset", async () => {
  await renderSettings("keybinds");
  const row = () => button(document.body, "Reset all").closest("section")!;
  const press = async (init: KeyboardEventInit) => {
    const input = document.querySelector<HTMLInputElement>(
      '[aria-label="New shortcut for Open Usage"]',
    )!;
    await act(async () =>
      input.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init })),
    );
  };
  await click(
    document.querySelector<HTMLButtonElement>('[aria-label="Add a shortcut to Open Usage"]')!,
  );
  await press({ key: "s", code: "KeyS", metaKey: true });
  expect(row().querySelector('[role="alert"]')!.textContent).toBe(
    "⌘S already runs Toggle sidebar.",
  );
  await press({ key: "y", code: "KeyY", metaKey: true });
  const usage = (init: KeyboardEventInit) => appShortcut(new KeyboardEvent("keydown", init));
  expect(usage({ code: "KeyY", metaKey: true })).toBe("usage");

  await click(
    document.querySelector<HTMLButtonElement>('[aria-label="Remove ⌥⌘U from Open Usage"]')!,
  );
  expect(usage({ code: "KeyU", metaKey: true, altKey: true })).toBeUndefined();
  await click(button(row(), "Reset all"));
  expect(usage({ code: "KeyU", metaKey: true, altKey: true })).toBe("usage");
  expect(usage({ code: "KeyY", metaKey: true })).toBeUndefined();
});

test("Source control shows the host's GitHub CLI and its account", async () => {
  states["local"] = {
    status: "connected",
    plxd: "0.1.0",
    protocol: 1,
    capabilities: { githubStatus: {} },
  };
  answers["github/status"] = () => ({
    result: { installed: true, version: "2.100.0", signedIn: true, account: "ryan", checkedAt: "" },
  });
  await renderSettings("sourceControl");
  expect(rows("Hosting")).toEqual(["GitHubgh 2.100.0Signed in as @ryan"]);
  states["local"] = { status: "connected", plxd: "0.1.0", protocol: 1, capabilities: {} };
});

test("Source control on a plxd without githubSetup links to gh's install page", async () => {
  states["local"] = {
    status: "connected",
    plxd: "0.1.0",
    protocol: 1,
    capabilities: { githubStatus: {} },
  };
  answers["github/status"] = () => ({ result: { installed: false, checkedAt: "" } });
  await renderSettings("sourceControl");
  expect(rows("Hosting")).toEqual([
    "GitHubNot installed. Install the GitHub CLI on this host to open pull requests.Install",
  ]);
  expect(section("Hosting").querySelector("a")!.href).toBe("https://cli.github.com/");
  states["local"] = { status: "connected", plxd: "0.1.0", protocol: 1, capabilities: {} };
});

test("Source control installs gh, signs it in with a code, and follows the browser's approval", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
  states["local"] = {
    status: "connected",
    plxd: "0.1.0",
    protocol: 1,
    capabilities: { githubStatus: {}, githubSetup: {} },
  };
  const signIn = { code: "AB12-CD34", url: "https://github.com/login/device", expiresAt: "" };
  let status: Record<string, unknown> = { installed: false, checkedAt: "" };
  answers["github/status"] = () => ({ result: status });
  answers["github/install"] = () => {
    status = { installed: false, installing: true, checkedAt: "" };
    return { result: status };
  };
  answers["github/signIn"] = () => {
    status = { ...status, signingIn: signIn };
    return { result: signIn };
  };
  answers["github/signInCancel"] = () => {
    status = { ...status, signingIn: undefined };
    return { result: {} };
  };
  const open = vi.fn();
  window.open = open;
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  const poll = async () => {
    await act(() => vi.advanceTimersByTimeAsync(2000));
    await settle();
  };

  await renderSettings("sourceControl");
  expect(rows("Hosting")).toEqual([
    "GitHubNot installed. Parallax can install the GitHub CLI on this host.Install",
  ]);
  await click(button(section("Hosting"), "Install"));
  expect(rows("Hosting")).toEqual(["GitHubInstalling the GitHub CLI…Installing"]);

  status = { installed: true, version: "2.102.0", managed: true, signedIn: false, checkedAt: "" };
  await poll();
  expect(rows("Hosting")).toEqual([
    "GitHubgh 2.102.0 · installed by ParallaxNot signed in.Sign in",
  ]);

  await click(button(section("Hosting"), "Sign in"));
  expect(open).toHaveBeenCalledWith("https://github.com/login/device", "_blank");
  const [row, code] = rows("Hosting");
  expect(row).toBe(
    "GitHubgh 2.102.0 · installed by ParallaxWaiting for you to approve the code on GitHub…Cancel",
  );
  expect(code).toContain("Enter this code at github.com/login/device.");
  expect(section("Hosting").querySelector('[aria-label="One-time code"]')!.textContent).toBe(
    "AB12-CD34",
  );
  await click(button(section("Hosting"), "Copy"));
  expect(writeText).toHaveBeenCalledWith("AB12-CD34");

  // Cancel goes back to Sign in.
  await click(button(section("Hosting"), "Cancel"));
  expect(calls("github/signInCancel")).toHaveLength(1);
  expect(rows("Hosting")).toEqual([
    "GitHubgh 2.102.0 · installed by ParallaxNot signed in.Sign in",
  ]);

  // Approving in the browser shows on the next read, with setup-git's note.
  await click(button(section("Hosting"), "Sign in"));
  status = {
    installed: true,
    version: "2.102.0",
    managed: true,
    signedIn: true,
    account: "ryan",
    setupNote: "Signed in, but `gh auth setup-git` failed.",
    checkedAt: "",
  };
  await poll();
  expect(rows("Hosting")).toEqual([
    "GitHubgh 2.102.0 · installed by ParallaxSigned in as @ryanSigned in, but `gh auth setup-git` failed.",
  ]);
  // Nothing is pending, so polling stops.
  const reads = calls("github/status").length;
  await poll();
  expect(calls("github/status")).toHaveLength(reads);
  vi.unstubAllGlobals();
  states["local"] = { status: "connected", plxd: "0.1.0", protocol: 1, capabilities: {} };
});

test("Source control shows why an install failed, and offers Install again", async () => {
  states["local"] = {
    status: "connected",
    plxd: "0.1.0",
    protocol: 1,
    capabilities: { githubStatus: {}, githubSetup: {} },
  };
  answers["github/status"] = () => ({
    result: {
      installed: false,
      setupNote: "The downloaded gh_2.102.0_macOS_arm64.zip doesn't match gh's checksum.",
      checkedAt: "",
    },
  });
  await renderSettings("sourceControl");
  expect(rows("Hosting")).toEqual([
    "GitHubNot installed. Parallax can install the GitHub CLI on this host.The downloaded gh_2.102.0_macOS_arm64.zip doesn't match gh's checksum.Install",
  ]);
  states["local"] = { status: "connected", plxd: "0.1.0", protocol: 1, capabilities: {} };
});

test("Storage deletes only a host's archived threads, after asking", async () => {
  window.parallax.storage = async () => [];
  answers["thread/list"] = () => ({
    result: {
      repos: [],
      threads: [
        { id: "t-old", repo: "r", archived: true, createdAt: "" },
        { id: "t-live", repo: "r", createdAt: "" },
      ],
    },
  });
  answers["thread/delete"] = () => ({ result: {} });
  await renderSettings("storage");
  const archived = section("Archived threads");
  expect(archived.textContent).toContain("1 archived thread");
  await click(button(archived, "Delete all"));
  expect(calls("thread/delete")).toEqual([]);
  await click(button(archived, "Delete all"));
  expect(calls("thread/delete")).toEqual([{ host: "local", params: { runId: "t-old" } }]);
});

test("Connections renames this computer", async () => {
  const renameLocal = vi.fn(async () => undefined);
  window.parallax.renameLocal = renameLocal;
  await renderSettings("connections");
  await click(document.querySelector<HTMLButtonElement>('[aria-label="Rename this computer"]')!);
  const input = document.querySelector<HTMLInputElement>('[aria-label="Computer name"]')!;
  expect(input.value).toBe("This Mac");
  input.value = "macbook";
  await act(async () => input.form!.requestSubmit());
  expect(renameLocal).toHaveBeenCalledWith("macbook");
});

test("Typography's sizes and Word wrap are kept, and Advanced takes any font's name", async () => {
  await renderSettings("appearance");
  const select = (label: string) =>
    document.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!;
  change(select("Interface font size"), "16");
  change(select("Monospace font size"), "14");
  await click(
    document.querySelector<HTMLButtonElement>('[role="switch"][aria-label="Word wrap"]')!,
  );
  await click(document.querySelector<HTMLButtonElement>('[role="switch"][aria-label="Advanced"]')!);
  change(
    document.querySelector<HTMLInputElement>('input[aria-label="Monospace font"]')!,
    "Berkeley Mono",
  );
  expect(JSON.parse(localStorage.getItem("parallax.appearance")!)).toMatchObject({
    uiSize: 16,
    codeSize: 14,
    wordWrap: false,
    codeFont: "Berkeley Mono",
  });
  localStorage.removeItem("parallax.appearance");
});

test("the last provider on can't be turned off", async () => {
  localStorage.setItem("parallax.disabledProviders", JSON.stringify(["codex", "cursor"]));
  vi.resetModules();
  const { Settings: Fresh } = await import("./Settings");
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  act(() =>
    root.render(<Fresh section="providers" listed={[]} theme="system" onThemeChange={() => {}} />),
  );
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
  };
  await settle();
  const toggle = (name: string) =>
    document.querySelector<HTMLButtonElement>(`[role="switch"][aria-label="Use ${name}"]`)!;
  expect(toggle("Claude Code").disabled).toBe(true);
  expect(toggle("Codex").disabled).toBe(false);
  localStorage.removeItem("parallax.disabledProviders");
});

test("Account saves a changed name, and shows main's answer when it fails", async () => {
  let publish = (_profile: Profile | null) => {};
  const ryan = { name: "Ryan Stoffel", firstName: "Ryan", lastName: "Stoffel", email: "r@x.dev" };
  const saveName = vi.fn(async () => "Network error" as string | undefined);
  Object.assign(window.parallax, {
    onProfile: (listener: (profile: Profile | null) => void) => {
      publish = listener;
      listener(ryan);
      return () => {};
    },
    saveName,
  });
  await renderSettings("account");
  const first = document.querySelector<HTMLInputElement>('input[aria-label="First name"]')!;
  const save = button(section("Account settings"), "Save");
  expect(first.value).toBe("Ryan");
  expect(save.disabled).toBe(true);

  change(first, " Ry ");
  expect(save.disabled).toBe(false);
  await click(save);
  expect(saveName).toHaveBeenCalledWith(" Ry ", "Stoffel");
  expect(section("Account settings").textContent).toContain("Network error");

  // Once main publishes the saved name, the fields start over from it.
  saveName.mockResolvedValue(undefined);
  await click(save);
  act(() => publish({ ...ryan, name: "Ry Stoffel", firstName: "Ry" }));
  expect(document.querySelector<HTMLInputElement>('input[aria-label="First name"]')!.value).toBe(
    "Ry",
  );
  expect(button(section("Account settings"), "Save").disabled).toBe(true);
  expect(section("Profile").querySelector("h2")!.textContent).toBe("Ry Stoffel");
});
