/**
 * Every command Fulcrum has, with its default chords.
 *
 * This table is the "defaults" half of the keybinding registry (`lib/keymap.ts`);
 * `Settings::keybindings` supplies the user's overrides. The command palette lists the
 * same table, so every shortcut is discoverable and every command is reachable without
 * one.
 */
import { useEffect, useRef } from "react";
import { type CommandContext, type CommandDef, KeyDispatcher } from "../lib/keymap";
import type { Move } from "../lib/virtual";

const MOVES: [Move, string, string][] = [
  ["up", "ArrowUp", "Up"],
  ["down", "ArrowDown", "Down"],
  ["left", "ArrowLeft", "Left"],
  ["right", "ArrowRight", "Right"],
  ["pageUp", "PageUp", "Page up"],
  ["pageDown", "PageDown", "Page down"],
  ["home", "Home", "First item"],
  ["end", "End", "Last item"],
];

/**
 * Cursor movement, generated: plain moves the selection, Shift extends it, Ctrl moves
 * only the cursor (so Ctrl+Space can toggle rows without losing the selection).
 */
/** Vertical moves also drive lists that own a text field (palette, pickers); file-list
 *  handlers decline when `ctx.inInput`, so the caret keys still reach the field. */
const IN_INPUT_MOVES = new Set<Move>(["up", "down", "pageUp", "pageDown"]);

const MOVE_COMMANDS: CommandDef[] = MOVES.flatMap(([move, key, label]) => [
  { id: `list.${move}`, title: `Move: ${label}`, category: "List", keys: [key], hidden: true, allowInInput: IN_INPUT_MOVES.has(move) },
  { id: `list.${move}.extend`, title: `Extend selection: ${label}`, category: "List", keys: [`Shift+${key}`], hidden: true },
  { id: `list.${move}.cursor`, title: `Move cursor: ${label}`, category: "List", keys: [`Ctrl+${key}`], hidden: true },
]);

export const COMMANDS: CommandDef[] = [
  // Palette and app
  { id: "palette.files", title: "Go to file…", category: "General", keys: ["Ctrl+P"], allowInInput: true },
  { id: "palette.commands", title: "Show all commands", category: "General", keys: ["Ctrl+Shift+P"], allowInInput: true },
  { id: "app.settings", title: "Open settings", category: "General", keys: ["Ctrl+,"], allowInInput: true },
  { id: "app.keybindings", title: "Edit keyboard shortcuts", category: "General", keys: ["Ctrl+K"] },
  { id: "ui.cancel", title: "Cancel / clear", category: "General", keys: ["Escape"], allowInInput: true, hidden: true },
  { id: "index.enable", title: "Enable instant search", category: "General", keys: [] },

  // Search
  { id: "search.focus", title: "Search in this folder", category: "Search", keys: ["Ctrl+F", "F3"], allowInInput: true },
  { id: "search.everywhere", title: "Search everywhere", category: "Search", keys: ["Ctrl+Shift+F"], allowInInput: true },

  // Navigation
  { id: "nav.back", title: "Back", category: "Navigate", keys: ["Alt+ArrowLeft", "Backspace"] },
  { id: "nav.forward", title: "Forward", category: "Navigate", keys: ["Alt+ArrowRight"] },
  { id: "nav.up", title: "Up one level", category: "Navigate", keys: ["Alt+ArrowUp"] },
  { id: "nav.refresh", title: "Refresh", category: "Navigate", keys: ["F5", "Ctrl+R"] },
  { id: "nav.editPath", title: "Edit path", category: "Navigate", keys: ["Ctrl+L", "Alt+D"], allowInInput: true },

  // Tabs
  { id: "tab.new", title: "New tab", category: "Tabs", keys: ["Ctrl+T"], allowInInput: true },
  { id: "tab.close", title: "Close tab", category: "Tabs", keys: ["Ctrl+W"], allowInInput: true },
  { id: "tab.reopen", title: "Reopen closed tab", category: "Tabs", keys: ["Ctrl+Shift+T"], allowInInput: true },
  { id: "tab.next", title: "Next tab", category: "Tabs", keys: ["Ctrl+Tab"], allowInInput: true },
  { id: "tab.prev", title: "Previous tab", category: "Tabs", keys: ["Ctrl+Shift+Tab"], allowInInput: true },

  // Panes
  { id: "pane.splitVertical", title: "Split side by side", category: "Panes", keys: ["Ctrl+\\"] },
  { id: "pane.splitHorizontal", title: "Split top and bottom", category: "Panes", keys: ["Ctrl+Shift+\\"] },
  { id: "pane.single", title: "Single pane", category: "Panes", keys: ["Ctrl+Alt+\\"] },
  { id: "pane.focusOther", title: "Switch to other pane", category: "Panes", keys: ["Ctrl+`", "F6"], allowInInput: true },
  { id: "pane.copyToOther", title: "Copy selection to other pane", category: "Panes", keys: ["Shift+F5"] },
  { id: "pane.moveToOther", title: "Move selection to other pane", category: "Panes", keys: ["Shift+F6"] },
  { id: "pane.mirror", title: "Show this folder in other pane", category: "Panes", keys: ["Ctrl+Shift+M"] },

  // View
  { id: "view.details", title: "View: details", category: "View", keys: ["Ctrl+Shift+1"] },
  { id: "view.list", title: "View: list", category: "View", keys: ["Ctrl+Shift+2"] },
  { id: "view.tiles", title: "View: tiles", category: "View", keys: ["Ctrl+Shift+3"] },
  { id: "view.grid", title: "View: grid", category: "View", keys: ["Ctrl+Shift+4"] },
  { id: "view.preview", title: "Toggle preview pane", category: "View", keys: ["Alt+P"] },
  { id: "view.hidden", title: "Toggle hidden files", category: "View", keys: ["Ctrl+H"] },
  { id: "view.sidebar", title: "Toggle sidebar", category: "View", keys: ["Ctrl+B"] },
  { id: "view.density", title: "Cycle row density", category: "View", keys: [] },

  // Files
  { id: "list.open", title: "Open", category: "File", keys: ["Enter"], hidden: true, allowInInput: true },
  { id: "list.openNewTab", title: "Open in new tab", category: "File", keys: ["Ctrl+Enter"] },
  { id: "list.selectAll", title: "Select all", category: "Selection", keys: ["Ctrl+A"] },
  { id: "list.invert", title: "Invert selection", category: "Selection", keys: ["Ctrl+I"] },
  { id: "list.toggle", title: "Toggle selection of focused item", category: "Selection", keys: ["Ctrl+Space"], hidden: true },
  { id: "file.rename", title: "Rename", category: "File", keys: ["F2"] },
  { id: "file.trash", title: "Move to Recycle Bin", category: "File", keys: ["Delete"] },
  { id: "file.delete", title: "Delete permanently", category: "File", keys: ["Shift+Delete"] },
  { id: "file.copy", title: "Copy", category: "File", keys: ["Ctrl+C"] },
  { id: "file.cut", title: "Cut", category: "File", keys: ["Ctrl+X"] },
  { id: "file.paste", title: "Paste", category: "File", keys: ["Ctrl+V"] },
  { id: "file.newFolder", title: "New folder", category: "File", keys: ["Ctrl+Shift+N"] },
  { id: "file.copyPath", title: "Copy path", category: "File", keys: ["Ctrl+Shift+C"] },
  { id: "file.pin", title: "Pin folder to sidebar", category: "File", keys: [] },
  { id: "file.reveal", title: "Show in system explorer", category: "File", keys: [] },
  { id: "file.properties", title: "Properties", category: "File", keys: ["Alt+Enter"] },

  ...MOVE_COMMANDS,
];

export const keys = new KeyDispatcher(COMMANDS);

/** True when keystrokes belong to a text field rather than to the app. */
export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target instanceof HTMLTextAreaElement) return true;
  if (target instanceof HTMLInputElement) {
    return !["checkbox", "radio", "button", "range", "submit"].includes(target.type);
  }
  return false;
}

/** The one keydown listener in the app. */
export function installKeyListener(): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (e.isComposing) return; // IME composition owns the keyboard
    if (keys.handle(e, isEditable(e.target))) {
      e.preventDefault();
      e.stopPropagation();
    }
  };
  window.addEventListener("keydown", onKey, { capture: true });
  return () => window.removeEventListener("keydown", onKey, { capture: true });
}

/**
 * Registers a handler for a command for the component's lifetime. The latest handler is
 * read through a ref, so callers can pass an inline closure without re-registering (and
 * reshuffling stack order) on every render.
 */
export function useCommand(
  id: string,
  handler: (ctx: CommandContext) => boolean | void,
  enabled = true,
): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    if (!enabled) return;
    return keys.on(id, (ctx) => ref.current(ctx));
  }, [id, enabled]);
}

/** Registers several handlers at once; same semantics as `useCommand`. */
export function useCommands(
  map: Record<string, (ctx: CommandContext) => boolean | void>,
  enabled = true,
): void {
  const ref = useRef(map);
  ref.current = map;
  const ids = Object.keys(map).join("|");
  useEffect(() => {
    if (!enabled) return;
    const offs = ids.split("|").map((id) => keys.on(id, (ctx) => ref.current[id]?.(ctx)));
    return () => offs.forEach((off) => off());
  }, [ids, enabled]);
}
