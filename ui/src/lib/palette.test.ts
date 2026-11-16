import { describe, expect, it } from "vitest";
import type { CommandDef } from "./keymap";
import { matchTitle, rankCommands } from "./palette";

const cmd = (id: string, title: string, hidden = false): CommandDef => ({ id, title, category: "X", keys: [], hidden });
const CMDS = [
  cmd("tab.new", "New tab"),
  cmd("file.paste", "Paste"),
  cmd("file.copy", "Copy"),
  cmd("pane.copy", "Copy selection to other pane"),
  cmd("list.down", "Move: Down", true),
];

describe("matchTitle", () => {
  it("prefers word starts and reports runs", () => {
    const m = matchTitle("nt", "New tab")!;
    expect(m.runs).toEqual([
      [0, 1],
      [4, 5],
    ]);
  });

  it("returns null when letters are missing", () => {
    expect(matchTitle("zz", "New tab")).toBeNull();
  });

  it("merges contiguous letters into one run", () => {
    expect(matchTitle("cop", "Copy")!.runs).toEqual([[0, 3]]);
  });
});

describe("rankCommands", () => {
  it("ranks word-start matches first and hides hidden commands", () => {
    const r = rankCommands("nt", CMDS).map((x) => x.cmd.id);
    expect(r[0]).toBe("tab.new");
    expect(r).not.toContain("list.down");
  });

  it("prefers the shorter exact title", () => {
    expect(rankCommands("copy", CMDS)[0]!.cmd.id).toBe("file.copy");
  });

  it("floats recent commands to the top of an empty query", () => {
    expect(rankCommands("", CMDS, ["file.paste"])[0]!.cmd.id).toBe("file.paste");
  });
});
