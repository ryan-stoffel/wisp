// @vitest-environment happy-dom
import type { TiptapEditorHTMLElement } from "@tiptap/react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vite-plus/test";

import type { ParallaxBridge } from "../preload/bridge";
import type { AgentRun, PromptImage } from "../protocol/generated/protocol";
import { Composer, type ComposerProps } from "./Composer";
import type { ImageCaps } from "./images";
import { setCliEnabled } from "./models";
import type { AttachThreads } from "./threadContext";
import { dragThread } from "./threadDrag";
import { emptyThreads } from "./threads";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// happy-dom has no popovers. The model menu's items are in the DOM either way.
HTMLElement.prototype.hidePopover = () => {};
// happy-dom decodes no images. Every image is small enough to send as it is.
vi.stubGlobal("createImageBitmap", async () => ({ width: 64, height: 48, close() {} }));
// The composer's image reads, so a test can wait for them: happy-dom reads a file on two chained
// timers, which a fixed wait races when the event loop stalls (PLX-277).
const reads = vi.hoisted(() => [] as Promise<unknown>[]);
vi.mock(import("./images"), async (importOriginal) => {
  const images = await importOriginal();
  return {
    ...images,
    readImage: (...args) => {
      const read = images.readImage(...args);
      reads.push(read);
      return read;
    },
  };
});

let unmount = () => {};
afterEach(() => {
  act(() => unmount());
  vi.useRealTimers();
});

// plxd's caps (RYA-191).
const caps: ImageCaps = { maxImages: 10, maxImageBytes: 5_242_880, maxTotalBytes: 6_291_456 };

function render(
  onSend: (text: string) => Promise<string | undefined>,
  // null: a plxd that takes no images.
  imageCaps: ImageCaps | null = caps,
  props: Partial<ComposerProps> = {},
) {
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  act(() =>
    root.render(<Composer onSend={onSend} imageCaps={imageCaps ?? undefined} {...props} />),
  );
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
  };
  const box = document.querySelector<TiptapEditorHTMLElement>(
    '[role="textbox"][aria-label="Message"]',
  )!;
  // Types as a keyboard does, a character at a time through the editor's input handling (where
  // its Markdown shortcuts live). Tiptap keeps the editor on its element for tests.
  const type = (text: string) =>
    act(() => {
      const { view } = box.editor!;
      for (const char of text) {
        const { from, to } = view.state.selection;
        const insert = () => view.state.tr.insertText(char, from, to);
        if (!view.someProp("handleTextInput", (f) => f(view, from, to, char, insert)))
          view.dispatch(insert());
      }
    });
  const press = (key: string, init: KeyboardEventInit = {}) =>
    act(async () => {
      box.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...init }));
    });
  // Pastes a clipboard with these files, and text of these types, then waits for the files to
  // be read.
  const paste = async (files: File[], text: Record<string, string> = {}) => {
    const data = new DataTransfer();
    for (const [format, value] of Object.entries(text)) data.setData(format, value);
    for (const file of files) data.items.add(file);
    act(() => {
      box.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
    });
    await read();
  };
  return { box, type, press, paste };
}

// Waits for the added files to be read and shown.
const read = () => act(() => Promise.all(reads.splice(0)));

// A 1×1 PNG, and what's sent for it.
const png =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
const pngFile = (name = "image.png") =>
  new File([Uint8Array.from(atob(png), (c) => c.charCodeAt(0))], name, { type: "image/png" });
const sentPng: PromptImage = { mediaType: "image/png", data: png };
const thumbnails = () =>
  [...document.querySelectorAll("form img")].map((img) => img.getAttribute("alt"));
const alert = () => document.querySelector('[role="alert"]')?.textContent;

test("Manual says its requests are denied when they can't come to the chat, and why (RYA-196)", () => {
  const manual = () =>
    [
      ...document.querySelectorAll('[role="menu"][aria-label="Access"] [role="menuitemradio"]'),
    ].find((o) => o.textContent?.startsWith("Manual"))!.textContent;
  const shown = (manualDenied?: "host" | "run") => {
    const root = createRoot(document.body.appendChild(document.createElement("div")));
    act(() => root.render(<Composer backend="claude" manualDenied={manualDenied} />));
    const text = manual();
    act(() => root.unmount());
    document.body.innerHTML = "";
    return text;
  };
  expect(shown()).toBe("ManualAsks you before edits and commands.");
  expect(shown("host")).toBe(
    "ManualAsks before edits and commands. This host's plxd can't show those requests, so they're denied.",
  );
  expect(shown("run")).toBe(
    "ManualAsks before edits and commands. This chat started before Parallax could show those requests, so they're denied.",
  );
});

test("a new thread's Cursor model starts it on Cursor, which takes no effort (0036)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { type, press } = render(onSend, caps, { newThread: true, backend: "claude" });
  const control = (label: string) => document.querySelector(`[aria-label="${label}"]`);
  const effort = () => document.querySelector('[aria-label^="Reasoning effort"]');
  expect(effort()).not.toBeNull();
  const menu = document.getElementById(
    control("Model: Claude Opus 5.5")!.getAttribute("popovertarget")!,
  )!;
  act(() => menu.querySelector<HTMLButtonElement>('button[aria-label="Cursor"]')!.click());
  const composer = [...menu.querySelectorAll<HTMLElement>('[role="menuitemradio"]')].find((m) =>
    m.textContent?.startsWith("Composer 2.5 Fast"),
  )!;
  await act(async () => composer.click());
  expect(effort()).toBeNull();
  type("Hello");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(
    "Hello",
    {
      model: "composer-2.5-fast",
      permission: "edit",
      account: { kind: "subscription", backend: "cursor" },
    },
    [],
    [],
  );
});

test("Markdown formats as you type and is sent as Markdown, with the text as typed", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = render(onSend);
  type("Two things:");
  await press("Enter", { shiftKey: true });
  type("- ");
  type("first");
  await press("Enter", { shiftKey: true });
  type("second");
  expect([...box.querySelectorAll("ul > li")].map((li) => li.textContent)).toEqual([
    "first",
    "second",
  ]);
  // Shift+Enter on an empty item leaves the list.
  await press("Enter", { shiftKey: true });
  await press("Enter", { shiftKey: true });
  type("rename foo_bar in <div>, **all** of it");
  expect(box.querySelector("strong")?.textContent).toBe("all");
  await press("Enter", { shiftKey: true });
  type("thanks");

  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(
    "Two things:\n\n- first\n- second\n\nrename foo_bar in <div>, **all** of it\nthanks",
    {},
    [],
    [],
  );
  expect(box.textContent).toBe("");
});

test("insert adds its text after what's typed, as the PR view's Ask a question does", async () => {
  const onSend = vi.fn(async () => undefined);
  const root = createRoot(document.body.appendChild(document.createElement("div")));
  const draw = (insert?: string) =>
    act(() => root.render(<Composer onSend={onSend} insert={insert} />));
  unmount = () => {
    root.unmount();
    document.body.innerHTML = "";
  };
  draw();
  const box = document.querySelector<TiptapEditorHTMLElement>('[role="textbox"]')!;
  act(() => void box.editor!.commands.setContent("About"));
  draw("https://github.com/me/app/pull/42 ");
  expect(box.textContent).toBe("About https://github.com/me/app/pull/42 ");
});

test("typed text that only looks like Markdown is sent as typed", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = render(onSend);
  type("rename __init__ and _private_, then a * b * c");
  await press("Enter", { shiftKey: true });
  type("--- a/file.ts");
  expect(box.querySelector("strong, em, hr")).toBeNull();

  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(
    "rename __init__ and _private_, then a * b * c\n--- a/file.ts",
    {},
    [],
    [],
  );
});

test("an ordered list's nested lines indent past its widest number", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, press } = render(onSend);
  act(() => {
    box.editor!.commands.setContent(
      '<ol start="9"><li><p>a</p></li><li><p>b</p><ul><li><p>c</p></li></ul></li></ol>',
    );
  });
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("9.  a\n10. b\n    - c", {}, [], []);
});

test("``` and Shift+Enter start a code block, where Enter adds a line and Cmd+Enter sends", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = render(onSend);
  type("```ts");
  await press("Enter", { shiftKey: true });
  type("let a = 1;");
  await press("Enter");
  type("a += 1;");
  expect(box.querySelector("pre")?.textContent).toBe("let a = 1;\na += 1;");
  expect(onSend).not.toHaveBeenCalled();

  await press("Enter", { metaKey: true });
  expect(onSend).toHaveBeenCalledWith("```ts\nlet a = 1;\na += 1;\n```", {}, [], []);
});

test("paste takes the plain text, its lines as they are", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, press } = render(onSend);
  const data = new DataTransfer();
  data.setData("text/html", '<h1 style="color: red">Error</h1><p>at <b>main</b></p>');
  data.setData("text/plain", "Error\n  at main\n\nfn __init__()");
  act(() => {
    box.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  });
  expect(box.querySelector("h1, b, strong, [style]")).toBeNull();

  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("Error\n  at main\n\nfn __init__()", {}, [], []);
});

test("copying within one block gives just its text", async () => {
  const { box, type, press } = render(vi.fn(async () => undefined));
  const copy = (from: number, to: number) => {
    const data = new DataTransfer();
    act(() => {
      box.editor!.commands.setTextSelection({ from, to });
      box.dispatchEvent(new ClipboardEvent("copy", { clipboardData: data, bubbles: true }));
    });
    return data.getData("text/plain");
  };
  type("```");
  await press("Enter", { shiftKey: true });
  type("let yVariable = 1;");
  // The code block's text starts at 1.
  expect(copy(5, 14)).toBe("yVariable");

  act(() => void box.editor!.commands.clearContent());
  type("- ");
  type("first **item** here");
  // In a list item's paragraph, at 3.
  expect(copy(9, 13)).toBe("**item**");
});

test("cut gives the Markdown as text, which pastes back the same", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = render(onSend);
  type("- ");
  type("first");
  await press("Enter", { shiftKey: true });
  type("second");
  const data = new DataTransfer();
  act(() => {
    box.editor!.commands.selectAll();
    box.dispatchEvent(new ClipboardEvent("cut", { clipboardData: data, bubbles: true }));
  });
  expect(data.getData("text/plain")).toBe("- first\n- second");
  expect(box.textContent).toBe("");

  act(() => {
    box.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  });
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("- first\n- second", {}, [], []);
});

test("a failed send puts the same text and images back, ahead of anything added meanwhile", async () => {
  let fail = (_error: string) => {};
  const onSend = vi.fn(
    (_text: string) => new Promise<string | undefined>((resolve) => (fail = resolve)),
  );
  const { box, type, press, paste } = render(onSend);
  type("- ");
  type("first");
  await paste([new File([Uint8Array.of(0x47, 0x49, 0x46)], "a.gif", { type: "image/gif" })]);
  await press("Enter");
  expect(thumbnails()).toEqual([]);
  type("more");
  await paste(Array.from({ length: 10 }, () => pngFile()));
  await act(async () => fail("plxd is busy"));
  expect(alert()).toBe("plxd is busy");
  expect(box.querySelector("ul")?.textContent).toBe("first");
  // Still at most 10: the one sent, then the first nine added meanwhile.
  expect(thumbnails()).toHaveLength(10);

  await press("Enter");
  const gif = { mediaType: "image/gif", data: "R0lG" };
  expect(onSend).toHaveBeenLastCalledWith(
    "- first\n\nmore",
    {},
    [gif, ...Array<PromptImage>(9).fill(sentPng)],
    [],
  );
});

test("a pasted image sits above the text, with its name nowhere in it, and is sent beside it", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press, paste } = render(onSend);
  // A copied image file's only text is its name.
  await paste([pngFile("Screenshot 2026-09-29.png")], {
    "text/plain": "Screenshot 2026-09-29.png",
  });
  await paste([pngFile(), pngFile()]);
  expect(thumbnails()).toEqual(["Image 1", "Image 2", "Image 3"]);
  expect(box.textContent).toBe("");
  act(() => document.querySelector<HTMLElement>('[aria-label="Remove image 2"]')!.click());
  expect(thumbnails()).toEqual(["Image 1", "Image 2"]);

  type("What's wrong here?");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("What's wrong here?", {}, [sentPng, sentPng], []);
  expect(thumbnails()).toEqual([]);

  // Images alone can be sent too.
  await paste([pngFile()]);
  await press("Enter");
  expect(onSend).toHaveBeenLastCalledWith("", {}, [sentPng], []);
});

test("an image copied from a browser, with its URL as text, pastes as the image", async () => {
  const { box, paste } = render(vi.fn(async () => undefined));
  const url = "https://example.com/cat-photo.png";
  await paste([pngFile()], {
    "text/plain": url,
    "text/html": `<meta charset="utf-8"><img src="${url}">`,
  });
  expect(thumbnails()).toEqual(["Image 1"]);
  expect(box.textContent).toBe("");
});

test("text copied from an app with a picture of itself pastes as text", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, paste } = render(onSend);
  await paste([pngFile()], {
    "text/plain": "A1\tB1",
    "text/html": "<table><tr><td>A1</td></tr></table>",
  });
  expect(thumbnails()).toEqual([]);
  expect(box.textContent).toBe("A1\tB1");
});

test("an image plxd can't take says why and isn't added", async () => {
  const { paste } = render(vi.fn(async () => undefined));
  await paste([new File(["<svg/>"], "logo.svg", { type: "image/svg+xml" })]);
  expect(thumbnails()).toEqual([]);
  expect(alert()).toBe("Only PNG, JPEG, GIF, and WebP images can be sent.");

  await paste(Array.from({ length: 11 }, () => pngFile()));
  expect(thumbnails()).toHaveLength(10);
  expect(alert()).toBe("A message takes at most 10 images.");
});

test("without plxd's image capability, adding an image says so", async () => {
  const { paste } = render(
    vi.fn(async () => undefined),
    null,
  );
  await paste([pngFile()]);
  expect(thumbnails()).toEqual([]);
  expect(alert()).toBe("This host's plxd can't take images.");
});

test("a picked image is a thumbnail, and any other file is a chip", async () => {
  render(vi.fn(async () => undefined));
  const picker = document.querySelector<HTMLInputElement>('input[type="file"]')!;
  const data = new DataTransfer();
  data.items.add(pngFile());
  data.items.add(new File(["# Notes"], "notes.md", { type: "text/markdown" }));
  Object.defineProperty(picker, "files", { value: data.files });
  act(() => void picker.dispatchEvent(new Event("change", { bubbles: true })));
  await read();
  expect(thumbnails()).toEqual(["Image 1"]);
  expect(document.querySelector('[aria-label="Remove notes.md"]')).not.toBeNull();
});

test("Up and Down recall the thread's earlier prompts until one is edited (PLX-325)", async () => {
  const { box, type, press } = render(async () => undefined, caps, {
    history: ["first", "second\nline two"],
  });
  const shown = () => box.editor!.getText({ blockSeparator: "\n" });
  await press("ArrowUp");
  expect(shown()).toBe("second\nline two");
  await press("ArrowUp");
  expect(shown()).toBe("first");
  await press("ArrowUp");
  expect(shown()).toBe("first");
  await press("ArrowDown");
  expect(shown()).toBe("second\nline two");
  await press("ArrowDown");
  expect(shown()).toBe("");
  // An edited prompt is the user's own, so the arrows leave it be.
  await press("ArrowUp");
  type("!");
  await press("ArrowUp");
  expect(shown()).toBe("second\nline two!");
});

test("a recalled prompt sends as it was first sent, Markdown and all (PLX-325)", async () => {
  const prompt = "**all** of `it`\n- one\n- two";
  const onSend = vi.fn(async () => undefined);
  const { press } = render(onSend, caps, { history: [prompt] });
  await press("ArrowUp");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(prompt, {}, [], []);
});

// plxd's lists for the `/` and `@` menus (PLX-359): Claude's own `model` is the composer's.
const commandsResult = {
  logId: "log-1",
  result: {
    commands: ["ponytail:ponytail-help", "model", "review", "code-review"].map((name) => ({
      text: `/${name}`,
      name,
      description: `About ${name}`,
    })),
  },
};
const request = vi.fn(async (_host: string, method: string, _params?: unknown) =>
  method === "agent/commands"
    ? commandsResult
    : method === "thread/search"
      ? { logId: "log-1", result: { threads: [{ id: "t-flaky" }, { id: "t-docs" }] } }
      : {
          logId: "log-1",
          result: { files: ["README.md", "src/lib.rs", "src/main.rs"], truncated: false },
        },
);
const settle = async () => {
  for (let i = 0; i < 5; i++) await act(async () => {});
};
// The composer keeps each CLI's commands while the app runs, so each test gets its own run.
let runs = 0;
const withMenus = async (
  onSend: (text: string) => Promise<string | undefined>,
  props: Partial<ComposerProps> = {},
) => {
  request.mockClear();
  window.parallax = { platform: "darwin", request } as Partial<ParallaxBridge> as ParallaxBridge;
  const runId = `run-${++runs}`;
  const composer = render(onSend, caps, {
    backend: "claude",
    menus: { hostId: "local", runId },
    ...props,
  });
  await settle();
  // Typing settles too, for the lists fetched on the first `/` or `@`.
  const type = async (text: string) => {
    composer.type(text);
    await settle();
  };
  return { ...composer, type, runId };
};
const options = () =>
  [...document.querySelectorAll('[role="option"]')].map((o) => o.firstChild?.textContent);
const highlighted = () => document.querySelector('[aria-selected="true"]')?.firstChild?.textContent;
const loadingRow = () => document.querySelector('[role="listbox"] [role="status"]')?.textContent;

test("/ lists the composer's commands, then the CLI's, filtered as typed, and picks with the keyboard (PLX-359)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = await withMenus(onSend);
  await type("/");
  expect(options()).toEqual([
    "/model",
    "/effort",
    "/permissions",
    "/ponytail:ponytail-help",
    "/review",
    "/code-review",
  ]);
  // Names that start with it, then names that contain it.
  await type("rev");
  expect(options()).toEqual(["/review", "/code-review"]);
  expect(highlighted()).toBe("/review");
  await press("ArrowDown");
  expect(highlighted()).toBe("/code-review");
  await press("Tab");
  expect(box.textContent).toBe("/code-review ");
  expect(options()).toEqual([]);
  await type("the diff");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("/code-review the diff", expect.anything(), [], []);
});

test("commands are fetched on the first /, once per host, backend, and run, with a loading row meanwhile (PLX-359)", async () => {
  let answer: (result: typeof commandsResult) => void = () => {};
  request.mockImplementationOnce(() => new Promise((resolve) => (answer = resolve)));
  const { type, runId } = await withMenus(async () => undefined);
  expect(request).not.toHaveBeenCalled();
  await type("/");
  expect(request).toHaveBeenCalledExactlyOnceWith("local", "agent/commands", {
    backend: "claude",
    runId,
  });
  // The composer's own commands show at once.
  expect(options()).toEqual(["/model", "/effort", "/permissions"]);
  expect(loadingRow()).toBe("Loading commands…");
  await act(async () => answer(commandsResult));
  expect(options()).toHaveLength(6);
  expect(loadingRow()).toBeUndefined();
  act(() => unmount());

  // Opening the thread again lists them without starting the CLI.
  request.mockClear();
  const again = await withMenus(async () => undefined, {
    menus: { hostId: "local", runId },
  });
  await again.type("/");
  expect(request).not.toHaveBeenCalled();
  expect(options()).toHaveLength(6);
});

test("/model opens the composer's model picker instead of reaching the CLI (PLX-359)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = await withMenus(onSend);
  const opened = vi.fn();
  document.querySelector('[aria-label^="Model:"]')!.addEventListener("click", opened);
  await type("/mod");
  expect(options()).toEqual(["/model"]);
  await press("Enter");
  expect(opened).toHaveBeenCalledOnce();
  expect(box.textContent).toBe("");
  expect(onSend).not.toHaveBeenCalled();
});

test("@ lists the thread's files, fetched again on each new @, and inserts @path (PLX-359)", async () => {
  const { box, type, press, runId } = await withMenus(async () => undefined);
  expect(request).not.toHaveBeenCalled();
  await type("see @main");
  expect(request).toHaveBeenCalledExactlyOnceWith("local", "repo/files", { runId });
  expect(options()).toEqual(["src/main.rs"]);
  await press("Enter");
  expect(box.textContent).toBe("see @src/main.rs ");

  // A file the agent made since: the last list shows until the new one arrives.
  let answer: (files: string[]) => void = () => {};
  request.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        answer = (files) => resolve({ logId: "log-1", result: { files, truncated: false } });
      }),
  );
  await type("and @src/");
  expect(request).toHaveBeenCalledTimes(2);
  expect(options()).toEqual(["src/lib.rs", "src/main.rs"]);
  expect(loadingRow()).toBeUndefined();
  await act(async () => answer(["src/lib.rs", "src/main.rs", "src/new.rs"]));
  expect(options()).toEqual(["src/lib.rs", "src/main.rs", "src/new.rs"]);
});

test("Esc closes the menu, Enter then sends, and a plxd without composerMenus shows none (PLX-359)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { type, press } = await withMenus(onSend);
  await type("/rev");
  await press("Escape");
  expect(options()).toEqual([]);
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("/rev", expect.anything(), [], []);
  act(() => unmount());

  const { type: typeAgain } = await withMenus(onSend, { menus: undefined });
  await typeAgain("/");
  expect(request).not.toHaveBeenCalled();
  expect(options()).toEqual([]);
});

// The host's threads, for attaching (PLX-378): the open one, and three others on two providers.
const hostThreads = (self: string, max = 8): AttachThreads => {
  const thread = (id: string, lastPromptAt: string) => ({
    id,
    repo: "r",
    createdAt: lastPromptAt,
    lastPromptAt,
  });
  const run = (id: string, backend: string) => ({ id, backend }) as AgentRun;
  return {
    hostId: "local",
    state: {
      ...emptyThreads,
      threads: [
        thread("t-flaky", "2026-10-01T10:00:00Z"),
        thread("t-docs", "2026-10-03T10:00:00Z"),
        thread(self, "2026-10-03T11:00:00Z"),
        thread("t-ci", "2026-10-02T10:00:00Z"),
      ],
      titles: {
        "t-flaky": "Fix the flaky test",
        "t-docs": "Write the docs",
        "t-ci": "Speed up CI",
      },
      runs: { "t-flaky": run("t-flaky", "codex"), "t-docs": run("t-docs", "claude") },
    },
    open: vi.fn(),
    max,
    self,
  };
};
const rows = () => [...document.querySelectorAll('[role="option"]')].map((o) => o.textContent);
const groups = () =>
  [...document.querySelectorAll('[role="listbox"] [role="presentation"]')].map(
    (g) => g.textContent,
  );
const chips = () =>
  [...document.querySelectorAll("form [data-thread-chip]")].map((c) => c.textContent);

test("@ lists the host's threads above the files: its newest, then what thread/search finds, and attaches one as a chip (PLX-378)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { box, type, press } = await withMenus(onSend, { attach: hostThreads("run-self") });
  // With nothing typed, the newest, but not the open thread, with how long ago each was prompted.
  vi.useFakeTimers({ now: Date.parse("2026-10-03T12:00:00Z"), toFake: ["Date"] });
  await type("@");
  expect(groups()).toEqual(["Threads", "Files"]);
  expect(rows()).toEqual([
    "Write the docs2h",
    "Speed up CI1d",
    "Fix the flaky test2d",
    "README.md",
    "src/lib.rs",
    "src/main.rs",
  ]);
  expect(request).not.toHaveBeenCalledWith("local", "thread/search", expect.anything());

  // Typed, plxd's matches, and files only under their own heading.
  await type("flaky");
  expect(request).toHaveBeenCalledWith("local", "thread/search", { query: "flaky" });
  expect(rows()).toEqual(["Fix the flaky test2d", "Write the docs2h"]);
  await press("Enter");
  expect(box.textContent).toBe("");
  expect(chips()).toEqual(["Thread · 2dFix the flaky test"]);

  // An attached thread isn't offered again, and its chip comes off.
  await type("@docs");
  expect(rows()).toEqual(["Write the docs2h"]);
  await press("Enter");
  act(() =>
    document.querySelector<HTMLButtonElement>('[aria-label="Remove Fix the flaky test"]')!.click(),
  );
  expect(chips()).toEqual(["Thread · 2hWrite the docs"]);
  await type("summarize it");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith("summarize it", expect.anything(), [], ["t-docs"]);
  expect(chips()).toEqual([]);
});

// Drops a sidebar row's drag of thread `runId` on host `hostId` on the composer.
const drop = (hostId: string, runId: string) => {
  const data = new DataTransfer();
  dragThread(data, hostId, runId);
  // happy-dom's DragEvent takes no dataTransfer.
  const event = (type: string) =>
    Object.defineProperty(new Event(type, { bubbles: true }), "dataTransfer", { value: data });
  const form = document.querySelector("form")!;
  act(() => {
    form.dispatchEvent(event("dragover"));
    form.dispatchEvent(event("drop"));
  });
};

test("a sidebar row dropped on the box attaches its thread once, but not another computer's, the open one, or past the cap (PLX-378)", async () => {
  const onSend = vi.fn(async () => undefined);
  const { type, press } = await withMenus(onSend, { attach: hostThreads("run-self", 2) });
  drop("local", "t-flaky");
  drop("local", "t-flaky");
  drop("local", "run-self");
  expect(chips()).toHaveLength(1);
  expect(chips()[0]).toContain("Fix the flaky test");
  expect(alert()).toBeUndefined();

  drop("ssh-box", "t-docs");
  expect(alert()).toBe("A thread on another computer can't be attached here.");
  drop("local", "t-docs");
  drop("local", "t-ci");
  expect(alert()).toBe("A message takes at most 2 attached threads.");
  expect(chips()).toHaveLength(2);

  await type("compare these");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(
    "compare these",
    expect.anything(),
    [],
    ["t-flaky", "t-docs"],
  );
  act(() => unmount());

  // Without the capability, neither a drop nor `@` attaches anything.
  const without = await withMenus(onSend);
  drop("local", "t-flaky");
  await without.type("@flaky");
  expect(chips()).toEqual([]);
  expect(request).not.toHaveBeenCalledWith("local", "thread/search", expect.anything());
});

test("a provider turned off in Settings leaves the menu, and a new thread starts on the next", async () => {
  setCliEnabled("claude", false);
  const onSend = vi.fn(async () => undefined);
  const { type, press } = render(onSend, caps, { newThread: true, backend: "claude" });
  const menu = document.getElementById(
    document.querySelector('[aria-label="Model: GPT-6.1 Sol"]')!.getAttribute("popovertarget")!,
  )!;
  expect(menu.querySelector<HTMLButtonElement>('button[aria-label="Claude"]')!.disabled).toBe(true);
  type("Hello");
  await press("Enter");
  expect(onSend).toHaveBeenCalledWith(
    "Hello",
    expect.objectContaining({
      model: "gpt-6.1-sol",
      account: { kind: "subscription", backend: "codex" },
    }),
    [],
    [],
  );
  setCliEnabled("claude", true);
});
