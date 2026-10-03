import { merged, stored } from "./stored";
import type { KeyPress } from "./ui";

/** An app command a keybinding runs. App, ThreadList, and OpenMenu act on them. */
export type Command =
  | "newThread"
  | "noRepoThread"
  | "sidebar"
  | "panel"
  | "terminal"
  | "open"
  | "settings"
  | "usage"
  | "parentThread"
  | "nextThread"
  | "previousThread";

/**
 * Every rebindable command, in Settings > Keybinds' order, with its default bindings. "Mod" is
 * Cmd on macOS and Ctrl elsewhere. Ctrl+Shift+` is Ctrl on macOS too, so the terminal's toggle
 * is the same press everywhere.
 */
export const commands: { id: Command; name: string; defaults: string[] }[] = [
  { id: "newThread", name: "New thread", defaults: ["Mod+KeyN"] },
  { id: "noRepoThread", name: "New thread without a repository", defaults: ["Mod+Shift+KeyN"] },
  { id: "sidebar", name: "Toggle sidebar", defaults: ["Mod+KeyS"] },
  { id: "panel", name: "Toggle side panel", defaults: ["Mod+Alt+KeyB"] },
  { id: "terminal", name: "Toggle terminal", defaults: ["Mod+KeyJ", "Ctrl+Shift+Backquote"] },
  { id: "open", name: "Open folder in default app", defaults: ["Mod+Alt+KeyO"] },
  { id: "settings", name: "Open Settings", defaults: ["Mod+Comma"] },
  { id: "usage", name: "Open Usage", defaults: ["Mod+Alt+KeyU"] },
  // A thread's lineage (0041): from a parent, next and previous open its first and last child.
  { id: "parentThread", name: "Go to parent thread", defaults: ["Mod+Alt+ArrowUp"] },
  { id: "nextThread", name: "Next sibling thread", defaults: ["Mod+Alt+ArrowRight"] },
  { id: "previousThread", name: "Previous sibling thread", defaults: ["Mod+Alt+ArrowLeft"] },
];

const order = ["Ctrl", "Alt", "Shift", "Meta"];
export const modifiers = ["Control", "Alt", "Shift", "Meta", "AltGraph"];

/** `binding` with Mod made this OS's, and its modifiers in `keybindingOf`'s order. */
function resolve(binding: string): string {
  const mod = window.parallax.platform === "darwin" ? "Meta" : "Ctrl";
  const parts = binding.split("+").map((p) => (p === "Mod" ? mod : p));
  const key = parts.pop()!;
  return [...order.filter((m) => parts.includes(m)), key].join("+");
}

/**
 * The keybinding a press makes, as its modifiers then its key's code, e.g. "Ctrl+Shift+KeyT".
 * Undefined for a lone modifier, for a press without Cmd or Ctrl, which would type, and off macOS
 * for AltGr, which arrives as Ctrl+Alt and types characters.
 */
export function keybindingOf(e: KeyPress): string | undefined {
  if (modifiers.includes(e.key) || !(e.metaKey || e.ctrlKey)) return undefined;
  if (window.parallax.platform !== "darwin" && e.getModifierState("AltGraph")) return undefined;
  const held = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Meta"];
  return [...held.filter(Boolean), e.code].join("+");
}

const macSymbols: Record<string, string> = { Ctrl: "⌃", Alt: "⌥", Shift: "⇧", Meta: "⌘" };
const keyNames: Record<string, string> = {
  Comma: ",",
  Backquote: "`",
  Period: ".",
  Slash: "/",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
};

/** A keybinding as the OS writes it: "⌃⇧T" on macOS, "Ctrl+Shift+T" elsewhere. */
export function formatKeybinding(keybinding: string): string {
  const parts = keybinding.split("+");
  const code = parts.pop()!;
  const key = keyNames[code] ?? code.replace(/^(Key|Digit)/, "");
  return window.parallax.platform === "darwin"
    ? parts.map((p) => macSymbols[p]).join("") + key
    : [...parts, key].join("+");
}

/** A keybinding as `aria-keyshortcuts` writes it: "Meta+Shift+KeyN" is "Meta+Shift+N". */
export const ariaKeyshortcut = (keybinding: string) =>
  keybinding.replace(/^Ctrl\b/, "Control").replace(/(Key|Digit)(\w)$/, "$2");

// Cmd or Ctrl with these sends a message, or copies, pastes, cuts, undoes, or selects all.
export const editingKeys = ["Enter", "NumpadEnter", "KeyC", "KeyV", "KeyX", "KeyZ", "KeyA"];

type Overrides = Partial<Record<Command, string[]>>;
const overrides = stored<Overrides>("parallax.keybindings", {}, merged);

/** The bindings `command` runs on, this OS's: the user's, or else its defaults. */
export function bindingsOf(command: Command): string[] {
  return overrides.get()[command] ?? commands.find((c) => c.id === command)!.defaults.map(resolve);
}

/**
 * Sets `command`'s bindings, saved for every window of this computer. Undefined puts its
 * defaults back; an empty list leaves it with none.
 */
export function setBindings(command: Command, bindings: string[] | undefined) {
  const next = { ...overrides.get() };
  if (bindings) next[command] = bindings;
  else delete next[command];
  overrides.set(next);
}

/** Puts every command's defaults back. */
export const resetBindings = () => overrides.set({});

/** Whether `command`'s bindings are its defaults. */
export const isDefault = (command: Command) => !overrides.get()[command];

/** The command `keybinding` runs, if any. */
export const commandOf = (keybinding: string): Command | undefined =>
  commands.find((c) => bindingsOf(c.id).includes(keybinding))?.id;

/** Re-renders on any binding change. */
export const useKeybindings = overrides.use;

/** `command`'s first binding as the OS writes it, kept current, for tooltips and menus. */
export function useShortcutLabel(command?: Command): string | undefined {
  useKeybindings();
  const first = command && bindingsOf(command)[0];
  return first && formatKeybinding(first);
}
