import { describe, expect, it, vi } from "vitest";
import {
  buildKeymap,
  chordParts,
  type CommandDef,
  eventToChord,
  findConflicts,
  KeyDispatcher,
  type KeyLike,
  normalizeChord,
  setBinding,
  TYPEAHEAD,
} from "./keymap";

const key = (k: string, mods: Partial<KeyLike> = {}): KeyLike => ({
  key: k,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

const COMMANDS: CommandDef[] = [
  { id: "tab.new", title: "New tab", category: "Tabs", keys: ["Ctrl+T"], allowInInput: true },
  { id: "tab.close", title: "Close tab", category: "Tabs", keys: ["Ctrl+W"], allowInInput: true },
  { id: "file.delete", title: "Delete", category: "File", keys: ["Delete"] },
  { id: "list.down", title: "Down", category: "List", keys: ["ArrowDown"], hidden: true },
  { id: "nav.back", title: "Back", category: "Nav", keys: ["Alt+Left", "Backspace"] },
];

describe("normalizeChord", () => {
  it("orders modifiers canonically and uppercases letters", () => {
    expect(normalizeChord("shift+ctrl+p")).toBe("Ctrl+Shift+P");
    expect(normalizeChord("Meta+Alt+Ctrl+Shift+x")).toBe("Ctrl+Alt+Shift+Meta+X");
  });

  it("accepts the aliases people actually type", () => {
    expect(normalizeChord("control+esc")).toBe("Ctrl+Escape");
    expect(normalizeChord("cmd+up")).toBe("Meta+ArrowUp");
    expect(normalizeChord("alt+pgdn")).toBe("Alt+PageDown");
    expect(normalizeChord("f2")).toBe("F2");
    expect(normalizeChord("Ctrl+\\")).toBe("Ctrl+\\");
  });

  it("supports a literal plus key", () => {
    expect(normalizeChord("Ctrl++")).toBe("Ctrl++");
    expect(normalizeChord("+")).toBe("+");
  });

  it("rejects things that are not chords", () => {
    expect(normalizeChord("")).toBeNull();
    expect(normalizeChord("Ctrl")).toBeNull(); // lone modifier
    expect(normalizeChord("Ctrl+A+B")).toBeNull(); // two keys
    expect(normalizeChord("Ctrl+")).toBeNull();
    expect(normalizeChord("Hyper+K")).toBeNull();
    expect(normalizeChord("F99")).toBeNull();
  });
});

describe("eventToChord", () => {
  it("builds the same canonical form as normalizeChord", () => {
    expect(eventToChord(key("p", { ctrlKey: true, shiftKey: true }))).toBe("Ctrl+Shift+P");
    expect(eventToChord(key("ArrowLeft", { altKey: true }))).toBe("Alt+ArrowLeft");
    expect(eventToChord(key(" "))).toBe("Space");
  });

  it("binds shifted punctuation by its physical key", () => {
    expect(eventToChord(key("|", { ctrlKey: true, shiftKey: true, code: "Backslash" }))).toBe("Ctrl+Shift+\\");
    expect(eventToChord(key("\\", { ctrlKey: true, code: "Backslash" }))).toBe("Ctrl+\\");
  });

  it("ignores lone modifiers and dead keys", () => {
    expect(eventToChord(key("Control", { ctrlKey: true }))).toBeNull();
    expect(eventToChord(key("Shift", { shiftKey: true }))).toBeNull();
    expect(eventToChord(key("Dead"))).toBeNull();
  });

  it("uses the physical key when Alt composes a different character", () => {
    expect(eventToChord(key("π", { altKey: true, code: "KeyP" }))).toBe("Alt+P");
    expect(eventToChord(key("¡", { altKey: true, code: "Digit1" }))).toBe("Alt+1");
  });
});

describe("chordParts", () => {
  it("splits for display with friendly glyphs", () => {
    expect(chordParts("Ctrl+Shift+P")).toEqual(["Ctrl", "Shift", "P"]);
    expect(chordParts("Alt+ArrowUp")).toEqual(["Alt", "↑"]);
    expect(chordParts("Ctrl++")).toEqual(["Ctrl", "+"]);
  });
});

describe("buildKeymap", () => {
  it("indexes defaults both ways", () => {
    const km = buildKeymap(COMMANDS, []);
    expect(km.byChord.get("Ctrl+T")).toEqual(["tab.new"]);
    expect(km.byCommand.get("nav.back")).toEqual(["Alt+ArrowLeft", "Backspace"]);
  });

  it("lets an override replace every default chord of a command", () => {
    const km = buildKeymap(COMMANDS, [["nav.back", "Ctrl+["]]);
    expect(km.byCommand.get("nav.back")).toEqual(["Ctrl+["]);
    expect(km.byChord.has("Backspace")).toBe(false);
    expect(km.byChord.has("Alt+ArrowLeft")).toBe(false);
  });

  it("supports several override chords and an explicit unbind", () => {
    const km = buildKeymap(COMMANDS, [
      ["tab.new", "Ctrl+N"],
      ["tab.new", "ctrl+t"],
      ["file.delete", ""],
    ]);
    expect(km.byCommand.get("tab.new")).toEqual(["Ctrl+N", "Ctrl+T"]);
    expect(km.byCommand.get("file.delete")).toEqual([]);
    expect(km.byChord.has("Delete")).toBe(false);
  });

  it("ignores stale ids and malformed chords from old settings files", () => {
    const km = buildKeymap(COMMANDS, [
      ["removed.command", "Ctrl+Q"],
      ["tab.close", "Ctrl+Nope+X"],
    ]);
    expect(km.byChord.has("Ctrl+Q")).toBe(false);
    // A malformed-only override still counts as the user's choice: unbound.
    expect(km.byCommand.get("tab.close")).toEqual([]);
  });

  it("reports conflicts when a user binds one chord twice", () => {
    const km = buildKeymap(COMMANDS, [["file.delete", "Ctrl+T"]]);
    expect(findConflicts(km)).toEqual(new Map([["Ctrl+T", ["tab.new", "file.delete"]]]));
    expect(findConflicts(buildKeymap(COMMANDS, [])).size).toBe(0);
  });
});

describe("setBinding", () => {
  const back = COMMANDS[4]!;

  it("stores only the difference from defaults", () => {
    const next = setBinding([], back, ["Ctrl+["]);
    expect(next).toEqual([["nav.back", "Ctrl+["]]);
  });

  it("drops overrides when the user restores the defaults", () => {
    const next = setBinding([["nav.back", "Ctrl+["], ["tab.new", "Ctrl+N"]], back, ["alt+left", "Backspace"]);
    expect(next).toEqual([["tab.new", "Ctrl+N"]]);
  });

  it("records an unbind as an empty chord", () => {
    expect(setBinding([], back, [])).toEqual([["nav.back", ""]]);
  });
});

describe("KeyDispatcher", () => {
  it("runs the handler for a bound chord and reports it consumed", () => {
    const d = new KeyDispatcher(COMMANDS);
    const fn = vi.fn();
    d.on("tab.new", fn);
    expect(d.handle(key("t", { ctrlKey: true }), false)).toBe(true);
    expect(fn).toHaveBeenCalledOnce();
  });

  it("does not consume keys nobody handles, so the browser keeps them", () => {
    const d = new KeyDispatcher(COMMANDS);
    expect(d.handle(key("t", { ctrlKey: true }), false)).toBe(false);
    expect(d.handle(key("Tab"), false)).toBe(false);
  });

  it("gives the most recent handler priority and falls through when it declines", () => {
    const d = new KeyDispatcher(COMMANDS);
    const list = vi.fn();
    const palette = vi.fn(() => false as const);
    d.on("list.down", list);
    const off = d.on("list.down", palette);

    d.handle(key("ArrowDown"), false);
    expect(palette).toHaveBeenCalledOnce();
    expect(list).toHaveBeenCalledOnce(); // palette declined

    const paletteTakes = vi.fn();
    const off2 = d.on("list.down", paletteTakes);
    d.handle(key("ArrowDown"), false);
    expect(paletteTakes).toHaveBeenCalledOnce();
    expect(list).toHaveBeenCalledOnce(); // not reached this time

    off();
    off2();
    d.handle(key("ArrowDown"), false);
    expect(list).toHaveBeenCalledTimes(2);
  });

  it("suppresses non-input commands while a text field has focus", () => {
    const d = new KeyDispatcher(COMMANDS);
    const del = vi.fn();
    const tab = vi.fn();
    d.on("file.delete", del);
    d.on("tab.new", tab);
    expect(d.handle(key("Delete"), true)).toBe(false);
    expect(del).not.toHaveBeenCalled();
    expect(d.handle(key("t", { ctrlKey: true }), true)).toBe(true);
    expect(tab).toHaveBeenCalledOnce();
  });

  it("honours overrides live", () => {
    const d = new KeyDispatcher(COMMANDS);
    const fn = vi.fn();
    d.on("file.delete", fn);
    const changed = vi.fn();
    d.subscribe(changed);
    d.setOverrides([["file.delete", "Shift+Delete"]]);
    expect(changed).toHaveBeenCalledOnce();
    expect(d.handle(key("Delete"), false)).toBe(false);
    expect(d.handle(key("Delete", { shiftKey: true }), false)).toBe(true);
    expect(d.chordsFor("file.delete")).toEqual(["Shift+Delete"]);
  });

  it("routes unbound printable keys to type-ahead, but not chords or input", () => {
    const d = new KeyDispatcher(COMMANDS);
    const typed: string[] = [];
    d.on(TYPEAHEAD, (ctx) => {
      typed.push(ctx.text ?? "");
    });
    d.handle(key("r"), false);
    d.handle(key("e"), false);
    d.handle(key("r", { ctrlKey: true }), false);
    d.handle(key("x"), true);
    expect(typed).toEqual(["r", "e"]);
  });

  it("captures the next chord while recording, and Escape cancels", () => {
    const d = new KeyDispatcher(COMMANDS);
    const tab = vi.fn();
    d.on("tab.new", tab);
    const got: string[] = [];
    d.record((c) => got.push(c));
    expect(d.handle(key("t", { ctrlKey: true }), false)).toBe(true);
    expect(tab).not.toHaveBeenCalled(); // recorded, not dispatched
    d.record((c) => got.push(c));
    d.handle(key("Escape"), false);
    expect(got).toEqual(["Ctrl+T", ""]);
  });

  it("tolerates a handler unregistering itself mid-dispatch", () => {
    const d = new KeyDispatcher(COMMANDS);
    const off = d.on("tab.close", () => {
      off();
    });
    expect(() => d.run("tab.close")).not.toThrow();
    expect(d.run("tab.close")).toBe(false);
  });
});
