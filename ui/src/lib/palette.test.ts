import { describe, expect, it } from "vitest";
import type { SearchHit } from "../api/types";
import type { CommandDef } from "./keymap";
import { isCloseMatch, matchTitle, rankCommands, rankFileHits } from "./palette";

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

/* ───────────────────────────── File results ───────────────────────────── */

const hit = (name: string, ranges: number[] = [], path = `D:\\x\\${name}`): SearchHit =>
  ({ entry: { name }, path, volume: "D", score: 0, match_ranges: ranges }) as unknown as SearchHit;

describe("rankFileHits", () => {
  it("puts the file the user means first: exact, then stem, prefix, word start, anywhere", () => {
    const ranked = rankFileHits(
      [
        hit("MissionModule_zSubmodule_zadanie_Base.uasset"),
        hit("a_zadanie_notes.txt"),
        hit("zadanie_final.docx"),
        hit("zadanie.txt"),
        hit("zadanie"),
        hit("xzadanie.dds"),
      ],
      "zadanie",
    ).map((h) => h.entry.name);
    expect(ranked).toEqual(["zadanie", "zadanie.txt", "zadanie_final.docx", "a_zadanie_notes.txt", "MissionModule_zSubmodule_zadanie_Base.uasset", "xzadanie.dds"]);
  });

  it("is case-insensitive and prefers shorter names within a tier", () => {
    const ranked = rankFileHits([hit("Report_final_v2.pdf"), hit("report.pdf"), hit("REPORT_2.pdf")], "Report").map((h) => h.entry.name);
    expect(ranked).toEqual(["report.pdf", "REPORT_2.pdf", "Report_final_v2.pdf"]);
  });

  it("does not mutate its input", () => {
    const input = [hit("b.txt"), hit("a.txt")];
    rankFileHits(input, "txt");
    expect(input.map((h) => h.entry.name)).toEqual(["b.txt", "a.txt"]);
  });
});

describe("isCloseMatch", () => {
  it("accepts a query matched in one or two contiguous pieces (a typo makes two)", () => {
    expect(isCloseMatch(hit("zadanie.txt", [0, 7]))).toBe(true);
    expect(isCloseMatch(hit("zadanie.txt", [0, 3, 4, 7]))).toBe(true);
  });

  it("rejects letters scattered across a long name, and hits without ranges", () => {
    expect(isCloseMatch(hit("MissionModule_zSubmodule.uasset", [8, 9, 15, 16, 20, 21, 24, 25, 28, 29]))).toBe(false);
    expect(isCloseMatch(hit("zadanie.txt", []))).toBe(false);
  });
});
