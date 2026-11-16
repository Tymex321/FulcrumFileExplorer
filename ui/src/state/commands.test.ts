import { describe, expect, it } from "vitest";
import { buildKeymap, findConflicts, normalizeChord } from "../lib/keymap";
import { COMMANDS } from "./commands";

describe("default command table", () => {
  it("has no conflicting defaults", () => {
    expect(findConflicts(buildKeymap(COMMANDS, []))).toEqual(new Map());
  });

  it("has unique ids", () => {
    const ids = COMMANDS.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("writes every default chord in a form the parser accepts", () => {
    for (const c of COMMANDS) for (const k of c.keys) expect(normalizeChord(k), `${c.id}: ${k}`).not.toBeNull();
  });

  it("lets only modifier chords, Escape and list navigation fire inside text fields", () => {
    // A plain key (Delete, Backspace, a letter) allowed in inputs would eat typing. The
    // vertical list moves and Enter are the deliberate exception: the palette's input
    // needs them, and the file list's handlers decline when focus is in a field.
    const NAV = new Set(["list.up", "list.down", "list.pageUp", "list.pageDown", "list.open"]);
    for (const c of COMMANDS.filter((c) => c.allowInInput && !NAV.has(c.id))) {
      for (const k of c.keys) {
        const chord = normalizeChord(k)!;
        const plain = !/^(Ctrl|Alt|Meta)\+/.test(chord) && !/^F\d+$/.test(chord) && chord !== "Escape";
        expect(plain, `${c.id} binds ${chord} in inputs`).toBe(false);
      }
    }
  });

  it("binds the shortcuts the brief requires", () => {
    const km = buildKeymap(COMMANDS, []);
    expect(km.byChord.get("Ctrl+T")).toEqual(["tab.new"]);
    expect(km.byChord.get("Ctrl+W")).toEqual(["tab.close"]);
    expect(km.byChord.get("Ctrl+P")).toEqual(["palette.files"]);
    expect(km.byChord.get("Ctrl+Shift+P")).toEqual(["palette.commands"]);
    expect(km.byChord.get("Ctrl+A")).toEqual(["list.selectAll"]);
  });
});
