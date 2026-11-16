/**
 * The keybinding registry.
 *
 * One map of command id → chords → handler, and one window-level listener that consults
 * it. Nothing else in the app calls `addEventListener("keydown")` or uses `onKeyDown` for
 * shortcuts: scattered handlers are how two features silently claim the same chord and
 * the conflict becomes impossible to find, let alone let a user fix.
 *
 * Three ideas carry the design:
 *
 * - **Defaults live here, overrides live in settings.** `Settings::keybindings` records only
 *   what the user changed, so improving a default reaches everyone who has not touched it.
 * - **Handlers stack.** Several components may handle one command; the most recently
 *   registered handler that does not decline wins. The command palette registers
 *   `list.down` while it is open, so arrow keys move its cursor and fall back to the file
 *   list when it closes — no focus juggling and no second shortcut table.
 * - **Text fields are respected.** While focus is in an input, only commands explicitly
 *   marked `allowInInput` fire, so typing a path never triggers Delete or Backspace-up.
 */

export interface CommandDef {
  id: string;
  title: string;
  category: string;
  /** Default chords, normalised or not. Empty means unbound by default. */
  keys: string[];
  /** Fire even while a text field has focus (palette toggles, Escape). */
  allowInInput?: boolean;
  /** Hide from the command palette — for commands that only make sense as keys. */
  hidden?: boolean;
}

export interface KeyLike {
  key: string;
  code?: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  repeat?: boolean;
}

export interface CommandContext {
  /** True when the keystroke came from a text field. */
  inInput: boolean;
  /** Where the command came from, so handlers can differ for palette vs. key. */
  source: "key" | "palette" | "menu";
  /** The printable character, for the type-ahead pseudo-command. */
  text?: string;
  repeat?: boolean;
}

/** Return `false` to decline, letting the next handler (or the browser) have the key. */
export type Handler = (ctx: CommandContext) => boolean | void;

/** Pseudo-command receiving unbound printable keys, for type-to-select. */
export const TYPEAHEAD = "__typeahead__";

const MODIFIERS = ["Ctrl", "Alt", "Shift", "Meta"] as const;
type Modifier = (typeof MODIFIERS)[number];

const MOD_ALIASES: Record<string, Modifier> = {
  ctrl: "Ctrl",
  control: "Ctrl",
  alt: "Alt",
  option: "Alt",
  shift: "Shift",
  meta: "Meta",
  cmd: "Meta",
  command: "Meta",
  win: "Meta",
  super: "Meta",
};

const KEY_ALIASES: Record<string, string> = {
  esc: "Escape",
  escape: "Escape",
  del: "Delete",
  delete: "Delete",
  ins: "Insert",
  insert: "Insert",
  return: "Enter",
  enter: "Enter",
  space: "Space",
  " ": "Space",
  spacebar: "Space",
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
  arrowup: "ArrowUp",
  arrowdown: "ArrowDown",
  arrowleft: "ArrowLeft",
  arrowright: "ArrowRight",
  pgup: "PageUp",
  pageup: "PageUp",
  pgdn: "PageDown",
  pagedown: "PageDown",
  home: "Home",
  end: "End",
  tab: "Tab",
  backspace: "Backspace",
};

function normalizeKey(raw: string): string | null {
  if (!raw) return null;
  if (raw.length === 1) return raw === " " ? "Space" : raw.toUpperCase();
  const alias = KEY_ALIASES[raw.toLowerCase()];
  if (alias) return alias;
  if (/^f([1-9]|1[0-9]|2[0-4])$/i.test(raw)) return raw.toUpperCase();
  return null;
}

/**
 * Parses a user- or code-written chord into canonical form: modifiers in a fixed order,
 * then the key. `"shift+ctrl+p"` and `"Ctrl+Shift+P"` both become `"Ctrl+Shift+P"`.
 * Returns `null` for anything that is not a real chord, including a lone modifier.
 */
export function normalizeChord(input: string): string | null {
  const s = input.trim();
  if (!s) return null;
  // A literal "+" key ("Ctrl++") would otherwise split into an empty part.
  let parts: string[];
  if (s.endsWith("++")) {
    parts = s.slice(0, -2).split("+").filter(Boolean);
    parts.push("+");
  } else if (s === "+") {
    parts = ["+"];
  } else {
    parts = s.split("+").map((p) => p.trim());
  }
  if (parts.some((p) => p === "")) return null;

  const mods = new Set<Modifier>();
  let key: string | null = null;
  for (const part of parts) {
    const mod = MOD_ALIASES[part.toLowerCase()];
    if (mod) {
      mods.add(mod);
      continue;
    }
    if (key !== null) return null; // two non-modifier keys is not a chord
    key = normalizeKey(part);
    if (key === null) return null;
  }
  if (key === null) return null;
  return [...MODIFIERS.filter((m) => mods.has(m)), key].join("+");
}

const PUNCT_CODES: Record<string, string> = {
  Backslash: "\\",
  Slash: "/",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Backquote: "`",
};

/** The chord a keyboard event represents, or `null` for a lone modifier or dead key. */
export function eventToChord(e: KeyLike): string | null {
  if (["Control", "Shift", "Alt", "Meta", "Dead", "Unidentified", "Process"].includes(e.key)) {
    return null;
  }
  let key: string | null = null;
  // With Alt held, macOS and some Windows layouts report a composed character ("π" for
  // Alt+P). The physical code is the stable identity for letters and digits.
  const ascii = e.key.length === 1 && /[a-z0-9]/i.test(e.key);
  if (ascii) key = e.key.toUpperCase();
  else if (e.code && /^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (e.code && /^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  // Shift and Alt change what punctuation keys report ("|" for Shift+\); bind by the
  // unshifted key so "Ctrl+Shift+\\" written in the defaults matches the keystroke.
  else if ((e.shiftKey || e.altKey) && e.code && PUNCT_CODES[e.code]) key = PUNCT_CODES[e.code]!;
  else key = normalizeKey(e.key);
  if (key === null) return null;

  const mods: Modifier[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Meta");
  return [...mods, key].join("+");
}

/** Human-facing rendering of a chord, split for `<kbd>` styling. */
export function chordParts(chord: string): string[] {
  if (chord.endsWith("++")) return [...chord.slice(0, -2).split("+"), "+"];
  return chord.split("+").map((p) => KEY_LABELS[p] ?? p);
}

const KEY_LABELS: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Escape: "Esc",
  Delete: "Del",
  PageUp: "PgUp",
  PageDown: "PgDn",
  Backspace: "⌫",
};

export interface Keymap {
  /** chord → command ids bound to it (more than one means a conflict). */
  byChord: Map<string, string[]>;
  /** command id → its effective chords. */
  byCommand: Map<string, string[]>;
}

/**
 * Applies user overrides on top of the defaults.
 *
 * An override replaces *all* default chords for that command — that is what "rebind"
 * means to a user. Several override entries for one id give it several chords; an
 * override whose chord is empty unbinds the command entirely. Unknown ids and malformed
 * chords are ignored rather than thrown, because settings files outlive the commands
 * they mention.
 */
export function buildKeymap(
  commands: readonly CommandDef[],
  overrides: readonly (readonly [string, string])[],
): Keymap {
  const known = new Set(commands.map((c) => c.id));
  const overridden = new Map<string, string[]>();
  for (const [id, chord] of overrides) {
    if (!known.has(id)) continue;
    const list = overridden.get(id) ?? [];
    const norm = normalizeChord(chord);
    if (norm && !list.includes(norm)) list.push(norm);
    overridden.set(id, list);
  }

  const byCommand = new Map<string, string[]>();
  const byChord = new Map<string, string[]>();
  for (const cmd of commands) {
    const chords =
      overridden.get(cmd.id) ??
      cmd.keys.map(normalizeChord).filter((c): c is string => c !== null);
    byCommand.set(cmd.id, chords);
    for (const chord of chords) {
      const ids = byChord.get(chord) ?? [];
      ids.push(cmd.id);
      byChord.set(chord, ids);
    }
  }
  return { byChord, byCommand };
}

/** Chords claimed by more than one command. */
export function findConflicts(keymap: Keymap): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const [chord, ids] of keymap.byChord) if (ids.length > 1) out.set(chord, ids);
  return out;
}

/**
 * Produces the new override list after the user sets `chords` for `id`.
 *
 * Setting a command back to exactly its defaults removes its overrides, so settings stay
 * minimal and the user picks up future default improvements again.
 */
export function setBinding(
  overrides: readonly (readonly [string, string])[],
  command: CommandDef,
  chords: readonly string[],
): [string, string][] {
  const rest = overrides
    .filter(([id]) => id !== command.id)
    .map(([a, b]) => [a, b] as [string, string]);
  const norm = [...new Set(chords.map(normalizeChord).filter((c): c is string => !!c))];
  const defaults = command.keys.map(normalizeChord).filter((c): c is string => !!c);
  const same = norm.length === defaults.length && norm.every((c, i) => c === defaults[i]);
  if (same) return rest;
  if (norm.length === 0) return [...rest, [command.id, ""]];
  return [...rest, ...norm.map((c) => [command.id, c] as [string, string])];
}

/**
 * Owns the keymap and the handler stacks, and decides what a keystroke does.
 *
 * Deliberately free of DOM types so it can be driven by plain objects in tests; the React
 * hook that attaches it to `window` is a few lines in `state/keys.ts`.
 */
export class KeyDispatcher {
  private keymap: Keymap;
  private readonly handlers = new Map<string, Handler[]>();
  private readonly commands: Map<string, CommandDef>;
  private readonly listeners = new Set<() => void>();
  private overrides: readonly (readonly [string, string])[] = [];
  /** When set, the next keystroke is captured here instead of dispatched (rebinding UI). */
  private recorder: ((chord: string) => void) | null = null;

  constructor(readonly definitions: readonly CommandDef[]) {
    this.commands = new Map(definitions.map((c) => [c.id, c]));
    this.keymap = buildKeymap(definitions, []);
  }

  setOverrides(overrides: readonly (readonly [string, string])[]): void {
    this.overrides = overrides;
    this.keymap = buildKeymap(this.definitions, overrides);
    this.listeners.forEach((l) => l());
  }

  getOverrides(): readonly (readonly [string, string])[] {
    return this.overrides;
  }

  getKeymap(): Keymap {
    return this.keymap;
  }

  /** Subscribes to keymap changes (for the settings UI and palette chord hints). */
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  command(id: string): CommandDef | undefined {
    return this.commands.get(id);
  }

  chordsFor(id: string): string[] {
    return this.keymap.byCommand.get(id) ?? [];
  }

  /** Registers a handler on top of the stack for `id`. Returns the unregister function. */
  on(id: string, handler: Handler): () => void {
    const stack = this.handlers.get(id) ?? [];
    stack.push(handler);
    this.handlers.set(id, stack);
    return () => {
      const s = this.handlers.get(id);
      if (!s) return;
      const i = s.lastIndexOf(handler);
      if (i >= 0) s.splice(i, 1);
    };
  }

  /** Captures the next chord instead of dispatching it. */
  record(onChord: ((chord: string) => void) | null): void {
    this.recorder = onChord;
  }

  /** Runs a command's handler stack. Returns whether anything handled it. */
  run(id: string, ctx: CommandContext = { inInput: false, source: "palette" }): boolean {
    const stack = this.handlers.get(id);
    if (!stack) return false;
    // Iterate a copy: a handler may unregister itself (closing a menu) while running.
    const snapshot = [...stack];
    for (let i = snapshot.length - 1; i >= 0; i--) {
      if (snapshot[i]!(ctx) !== false) return true;
    }
    return false;
  }

  /**
   * Handles a keystroke. Returns `true` when it was consumed, in which case the caller
   * must `preventDefault()` — and must not when it returns `false`, or the browser loses
   * ordinary keys such as Tab and text input.
   */
  handle(e: KeyLike, inInput: boolean): boolean {
    const chord = eventToChord(e);
    if (chord === null) return false;

    if (this.recorder) {
      // Escape alone cancels recording rather than binding Escape by accident.
      const rec = this.recorder;
      this.recorder = null;
      rec(chord === "Escape" ? "" : chord);
      return true;
    }

    const ids = this.keymap.byChord.get(chord) ?? [];
    for (const id of ids) {
      const def = this.commands.get(id);
      if (inInput && !def?.allowInInput) continue;
      if (this.run(id, { inInput, source: "key", repeat: e.repeat })) return true;
    }

    // Type-to-select: an unbound printable key with no command modifiers.
    if (!inInput && !e.ctrlKey && !e.altKey && !e.metaKey && e.key.length === 1 && e.key !== " ") {
      return this.run(TYPEAHEAD, { inInput, source: "key", text: e.key });
    }
    return false;
  }
}
