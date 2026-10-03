import { Check, ChevronDown, Search } from "lucide-react";
import {
  Fragment,
  useEffect,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type ToggleEvent,
} from "react";

import { commandOf, keybindingOf, useShortcutLabel, type Command } from "./keybindings";

/**
 * A Mod shortcut as the OS writes it: "Alt+B" is "⌘⌥B" on macOS, "Ctrl+Alt+B" elsewhere, and
 * "Shift+N" is "⌘⇧N" on macOS.
 */
export const shortcut = (keys: string) =>
  window.parallax.platform === "darwin"
    ? `⌘${keys.replace("Alt+", "⌥").replace("Shift+", "⇧")}`
    : `Ctrl+${keys}`;

/** The parts of a key press shortcuts read, from a DOM or React keyboard event. */
export type KeyPress = Pick<
  globalThis.KeyboardEvent,
  "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "code" | "key" | "getModifierState"
>;

/**
 * The app's own shortcut `e` presses, if any: a command's binding (keybindings.ts), or Mod+1 to
 * Mod+9 a sidebar row (`rowShortcut`), which can't be rebound. App, ThreadList, and OpenMenu act
 * on them, and a repository action's keybinding can't be one.
 */
export function appShortcut(e: KeyPress): Command | "row" | undefined {
  if (rowShortcut(e) !== undefined) return "row";
  const keybinding = keybindingOf(e);
  return keybinding ? commandOf(keybinding) : undefined;
}

/**
 * Whether a press in a terminal is the app's alone. On macOS every app shortcut is. Elsewhere,
 * where they're Ctrl, a plain Ctrl+letter one (Ctrl+N, Ctrl+S) stays the shell's, which readline
 * and editors use; the terminal's toggle, Ctrl+1 to Ctrl+9, and any with Shift or Alt are the app's.
 */
export function terminalAppShortcut(e: KeyPress): boolean {
  const command = appShortcut(e);
  if (command === undefined) return false;
  const mac = window.parallax.platform === "darwin";
  return mac || command === "terminal" || command === "row" || e.shiftKey || e.altKey;
}

/** The 0-based row Mod+1 to Mod+9 picks, from the digit `e` presses with Mod and nothing else. */
export function rowShortcut(e: KeyPress): number | undefined {
  const digit = /^Digit([1-9])$/.exec(e.code)?.[1];
  const mod = window.parallax.platform === "darwin" ? e.metaKey : e.ctrlKey;
  return digit && mod && !e.shiftKey && !e.altKey ? Number(digit) - 1 : undefined;
}

/** Whether Mod is held, so lists can show their Mod+1 to Mod+9 badges. */
export function useModHeld() {
  const [held, setHeld] = useState(false);
  useEffect(() => {
    const mod = window.parallax.platform === "darwin" ? "Meta" : "Control";
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === mod) setHeld(e.type === "keydown");
    };
    // Switching apps with Cmd+Tab never sends its keyup.
    const blur = () => setHeld(false);
    window.addEventListener("keydown", key);
    window.addEventListener("keyup", key);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener("keyup", key);
      window.removeEventListener("blur", blur);
    };
  }, []);
  return held;
}

/** The ⌘1 to ⌘9 badge on a row that Mod and its number open. */
export function RowBadge({ index }: { index: number }) {
  return (
    <kbd className="shrink-0 rounded border border-border bg-surface px-1 font-sans text-[11px] leading-4 text-muted-foreground">
      {shortcut(String(index + 1))}
    </kbd>
  );
}

/**
 * A square, icon-only toolbar button. `label` is its accessible name and tooltip, with
 * `command`'s current shortcut; `aria-pressed` shows it on.
 */
export function IconButton({
  label,
  command,
  children,
  ...props
}: { label: string; command?: Command } & ButtonHTMLAttributes<HTMLButtonElement>) {
  const keys = useShortcutLabel(command);
  return (
    <button
      type="button"
      aria-label={label}
      title={keys ? `${label} (${keys})` : label}
      className="grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-hover hover:text-foreground aria-pressed:bg-selected aria-pressed:text-foreground [&_svg]:size-4"
      {...props}
    >
      {children}
    </button>
  );
}

/** A segmented control's option: a label around a visually hidden radio. */
export const segment =
  "flex items-center gap-1.5 rounded-md whitespace-nowrap px-2.5 py-1 text-[12.5px] text-muted-foreground hover:text-foreground has-checked:bg-selected has-checked:text-foreground has-focus-visible:outline-2 has-focus-visible:outline-ring [&_svg]:size-3.5";

/**
 * A row of radios drawn as one segmented control, named by `label`. Disabled, it stays in place
 * but can't be changed.
 */
export function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
  disabled,
}: {
  label: string;
  options: { value: T; name: string }[];
  value: T;
  onChange: (value: T) => void;
  disabled?: boolean;
}) {
  // Its own radio group, even when another control on the page has the same label.
  const name = useId();
  return (
    <fieldset
      aria-label={label}
      disabled={disabled}
      className="flex shrink-0 gap-0.5 rounded-lg border border-border p-0.5 disabled:pointer-events-none disabled:opacity-50"
    >
      {options.map((o) => (
        <label key={o.value} className={segment}>
          <input
            type="radio"
            name={name}
            value={o.value}
            checked={value === o.value}
            onChange={() => onChange(o.value)}
            className="sr-only"
          />
          {o.name}
        </label>
      ))}
    </fieldset>
  );
}

// Every dropdown shares these: a quiet trigger, and a panel 8px under it with the same radius.
/** A menu's trigger: sized to its content, highlighted on hover. */
export const menuButton =
  "flex items-center gap-1.5 rounded-lg py-1 pr-1.5 pl-2 text-[13.5px] text-muted-foreground enabled:hover:bg-hover enabled:hover:text-foreground disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0";

const panelAreas = {
  start: "[position-area:bottom_span-right]",
  end: "[position-area:bottom_span-left]",
  center: "[position-area:bottom]",
};

/**
 * A menu's panel, a native popover: under its trigger and lined up with its left edge (`end`:
 * its right edge; `center`: centered under it), flipping when there's no room. Escape and
 * clicking away close it.
 */
export const menuPanel = (align: "start" | "end" | "center" = "start") =>
  `inset-auto m-0 mt-2 rounded-lg border border-border bg-surface text-foreground shadow-composer [position-try-fallbacks:flip-block,flip-inline] ${panelAreas[align]}`;

/** A row in a menu panel. */
export const menuItem =
  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] hover:bg-hover [&_svg]:size-3.5 [&_svg]:shrink-0";

/**
 * Opens a menu on a right-click by clicking its trigger. Where the right-click arrives while a
 * button is still down (a right-click on macOS and Linux, Control-click on macOS), the menu opens
 * on its release, which would otherwise close it again as a click outside a popover. A release the
 * window never sees, as when the button is held while switching apps, opens nothing: the window's
 * blur, a cancelled pointer, or the next press ends the wait.
 */
export function openOnContextMenu(e: MouseEvent<HTMLElement>, trigger: HTMLElement | null) {
  e.preventDefault();
  if (e.buttons === 0) return trigger?.click();
  const wait = new AbortController();
  const { signal } = wait;
  window.addEventListener(
    "pointerup",
    () => {
      wait.abort();
      trigger?.click();
    },
    { signal },
  );
  for (const type of ["blur", "pointercancel", "pointerdown"])
    window.addEventListener(type, () => wait.abort(), { signal });
}

/** Up and Down move focus between a menu's items, wrapping at the ends and passing disabled ones. */
export function moveFocus(e: KeyboardEvent<HTMLElement>) {
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  const items = [
    ...e.currentTarget.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)'),
  ];
  const step = e.key === "ArrowDown" ? 1 : -1;
  const i = items.indexOf(document.activeElement as HTMLElement);
  // From outside the items (a search box), Down starts at the top and Up at the bottom.
  items.at(i === -1 ? (step === 1 ? 0 : -1) : (i + step) % items.length)?.focus();
}

export interface PickerOption {
  value: string;
  label: string;
  icon?: ReactNode;
  /** A line under the label. */
  description?: string;
  /** A quiet note at the row's end, such as "current". */
  hint?: string;
  /** Draws a line above this option, to set it apart. */
  divider?: boolean;
}

/**
 * One choice in a menu: icon, label, and an optional description and hint, checked when chosen.
 * A `disabled` one can't be picked, and its hint should say why.
 */
export function MenuOption({
  option: o,
  checked,
  disabled,
  onClick,
}: {
  option: PickerOption;
  checked: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={checked}
      disabled={disabled}
      onClick={onClick}
      className={`${menuItem} ${o.description ? "items-start py-2" : ""} disabled:opacity-50 disabled:hover:bg-transparent`}
    >
      {/* One line tall, so a two-line row's icon sits beside its label. */}
      {o.icon && <span className="grid h-5 place-items-center">{o.icon}</span>}
      <span className="min-w-0 flex-1">
        <span className="block truncate leading-5">{o.label}</span>
        {o.description && (
          <span className="mt-0.5 block text-[12px] leading-snug text-faint-foreground">
            {o.description}
          </span>
        )}
      </span>
      {o.hint && (
        <span className="shrink-0 text-[11.5px] leading-5 text-faint-foreground">{o.hint}</span>
      )}
      {/* Always takes its room, so choosing never rewraps a row or resizes the menu. */}
      <span className={`grid h-5 place-items-center ${checked ? "" : "invisible"}`}>
        <Check aria-hidden className="text-accent" />
      </span>
    </button>
  );
}

/** A small heading over a group of a menu's options. */
export const menuHeading = "px-2 pt-1.5 pb-1 text-[11.5px] font-medium text-faint-foreground";

/** The badge on a choice that can't be picked yet, saying why, such as "Not available yet". */
export const unavailableBadge =
  "shrink-0 rounded-md border border-amber-500/30 px-2 py-0.5 text-[12px] text-amber-500";

/**
 * A dropdown showing the chosen option, with a check beside it in the menu. The chosen option's
 * icon leads, else `icon`. Pass `value` to control it, or leave it to keep its own choice,
 * starting at `defaultValue`. `search` adds a filter box with that placeholder. Other buttons
 * can open the same menu with `popoverTarget={id}`; it anchors to whichever opened it, and
 * `button={false}` leaves them as its only way in.
 */
export function Picker({
  id,
  label,
  icon,
  options,
  value,
  defaultValue,
  onChange,
  align,
  search,
  panelClassName = "min-w-44",
  button = true,
}: {
  id?: string;
  button?: boolean;
  label: string;
  icon?: ReactNode;
  options: PickerOption[];
  value?: string;
  defaultValue?: string;
  onChange?: (value: string) => void;
  align?: "start" | "end";
  search?: string;
  panelClassName?: string;
}) {
  const ownId = useId();
  const menuId = id ?? ownId;
  const menu = useRef<HTMLDivElement>(null);
  const searchBox = useRef<HTMLInputElement>(null);
  const [own, setOwn] = useState(defaultValue);
  const [query, setQuery] = useState("");
  // Uncontrolled, it shows the first option until one is picked, even if options arrive later.
  const current =
    options.find((o) => o.value === (value ?? own)) ??
    (value === undefined ? options[0] : undefined);
  const q = query.trim().toLowerCase();
  const shown = q ? options.filter((o) => o.label.toLowerCase().includes(q)) : options;

  const choose = (o: PickerOption) => {
    menu.current?.hidePopover();
    setOwn(o.value);
    onChange?.(o.value);
  };

  return (
    <>
      {button && (
        <button
          type="button"
          popoverTarget={menuId}
          aria-haspopup="menu"
          aria-label={`${label}: ${current?.label ?? "none"}`}
          title={current?.label}
          // It can shrink, cutting a long choice (a branch name) short rather than widening its row.
          className={`${menuButton} min-w-0`}
        >
          {current?.icon ?? icon}
          <span className="truncate">{current?.label}</span>
          <ChevronDown aria-hidden className="opacity-70" />
        </button>
      )}
      <div
        ref={menu}
        id={menuId}
        popover="auto"
        role="menu"
        aria-label={label}
        onToggle={(e: ToggleEvent<HTMLDivElement>) => {
          if (e.newState === "closed") return setQuery("");
          if (search) searchBox.current?.focus();
          else menu.current?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus();
        }}
        onKeyDown={moveFocus}
        className={`${menuPanel(align)} overflow-hidden p-0 ${panelClassName}`}
      >
        {search && (
          <label className="flex items-center gap-2 border-b border-border px-3 py-2.5 focus-within:border-ring">
            <Search aria-hidden className="size-4 shrink-0 text-faint-foreground" />
            <input
              ref={searchBox}
              type="search"
              aria-label={search}
              placeholder={search}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && shown[0]) choose(shown[0]);
              }}
              className="min-w-0 flex-1 bg-transparent text-[13.5px] placeholder:text-faint-foreground focus-visible:outline-none"
            />
          </label>
        )}
        <div className="max-h-80 overflow-y-auto p-1">
          {shown.map((o) => (
            <Fragment key={o.value}>
              {o.divider && <div role="separator" className="-mx-1 my-1 h-px bg-border" />}
              <MenuOption option={o} checked={o === current} onClick={() => choose(o)} />
            </Fragment>
          ))}
          {shown.length === 0 && (
            <p className="px-2 py-1.5 text-[12.5px] text-faint-foreground">No matches</p>
          )}
        </div>
      </div>
    </>
  );
}

export interface Crumb {
  label: string;
  icon?: ReactNode;
  /**
   * Makes it a button to that page, such as back to a coordinator or on to New thread. The last
   * crumb with one isn't the current page.
   */
  onClick?: () => void;
}

/**
 * Where the user is: host, workspace, then the current page, split by slashes. `trail` follows
 * the last crumb after a `›`, such as a thread's children (Lineage.tsx).
 */
export function Breadcrumb({ items, trail }: { items: Crumb[]; trail?: ReactNode }) {
  return (
    // A trail takes the room the crumbs leave, and fits itself to it. In a pane too narrow for
    // the crumbs' minimum widths, they're cut off rather than slid under the top bar's buttons.
    <nav aria-label="Breadcrumb" className={`min-w-0 overflow-x-clip ${trail ? "flex-1" : ""}`}>
      <ol className="flex min-w-0 items-center gap-2 text-[13px]">
        {items.map(({ label, icon, onClick }, i) => {
          const last = i === items.length - 1;
          const current = last && !onClick;
          const Tag = onClick ? "button" : "span";
          // Before a trail, the last crumb keeps to 14rem, and gives way last.
          const room = last
            ? trail
              ? "max-w-56 min-w-20"
              : ""
            : i === 0
              ? "max-w-48 min-w-10 shrink-[100]"
              : "max-w-48 shrink-0";
          return (
            // The slash is CSS content, so it stays out of the crumb's text. Crumbs before the
            // last are cut short at 12rem, and the first, such as a computer's name, gives way
            // first when there's no room, so it never pushes the rest under the top bar's buttons.
            <li
              key={i}
              className={`flex min-w-0 items-center gap-2 ${room} ${i > 0 ? "before:text-faint-foreground before:content-['/']" : ""}`}
            >
              <Tag
                {...(onClick && { type: "button", onClick })}
                aria-current={current ? "page" : undefined}
                className={`flex min-w-0 items-center gap-1.5 [&_svg]:size-3.5 [&_svg]:shrink-0 ${current ? "font-medium text-foreground" : "text-muted-foreground"} ${onClick ? "rounded-md hover:text-foreground" : ""}`}
              >
                {icon}
                <span className="truncate" title={typeof label === "string" ? label : undefined}>
                  {label}
                </span>
              </Tag>
            </li>
          );
        })}
        {trail && (
          <li className="flex min-w-28 flex-1 items-center gap-1.5 before:text-faint-foreground before:content-['›']">
            {trail}
          </li>
        )}
      </ol>
    </nav>
  );
}

/** The 52px top row of a pane. On macOS and Windows it is also the window's title bar. */
export function TopBar({ className = "", children }: { className?: string; children: ReactNode }) {
  return (
    <div className={`titlebar flex h-13 shrink-0 items-center gap-2 px-3 ${className}`}>
      {children}
    </div>
  );
}
