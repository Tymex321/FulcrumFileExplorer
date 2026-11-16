import { describe, expect, it } from "vitest";
import { dropSlot, LISTING_COLUMNS, moveColumn, resizeColumn, slotToIndex, totalWidth, widthOf } from "./columns";
import { decodePayload, dropOperation, dropRejection, encodePayload } from "./dnd";
import { previewKind, typeInfo } from "./fileTypes";
import { EMPTY_TYPEAHEAD, feed, findMatch, TYPEAHEAD_RESET_MS } from "./typeahead";

describe("columns", () => {
  const layout = { order: [...LISTING_COLUMNS], widths: {} };

  it("clamps resizing to the column minimum", () => {
    expect(widthOf(resizeColumn(layout, "size", 10), "size")).toBe(64);
    expect(widthOf(resizeColumn(layout, "size", 200.4), "size")).toBe(200);
  });

  it("moves columns and converts drop slots into indices", () => {
    expect(moveColumn(["a", "b", "c", "d"] as never, 0, 2)).toEqual(["b", "c", "a", "d"]);
    // Dragging column 0 to slot 3 (between c and d) lands it at index 2.
    expect(slotToIndex(0, 3)).toBe(2);
    expect(slotToIndex(3, 1)).toBe(1);
  });

  it("finds the nearest boundary from widths alone", () => {
    const w = [100, 50, 50];
    expect(dropSlot(w, 10)).toBe(0);
    expect(dropSlot(w, 60)).toBe(1);
    expect(dropSlot(w, 130)).toBe(2);
    expect(dropSlot(w, 199)).toBe(3);
  });

  it("totals widths for horizontal scrolling", () => {
    expect(totalWidth({ order: ["name", "size"], widths: { name: 300 } })).toBe(396);
  });
});

describe("drag and drop", () => {
  it("moves within a volume, copies across, and modifiers override", () => {
    const none = { ctrl: false, shift: false };
    expect(dropOperation(["C:\\a\\x"], "C:\\b", none)).toBe("Move");
    expect(dropOperation(["C:\\a\\x"], "D:\\b", none)).toBe("Copy");
    expect(dropOperation(["C:\\a\\x"], "C:\\b", { ctrl: true, shift: false })).toBe("Copy");
    expect(dropOperation(["C:\\a\\x"], "D:\\b", { ctrl: false, shift: true })).toBe("Move");
  });

  it("refuses recursive and no-op drops", () => {
    expect(dropRejection(["C:\\a"], "C:\\a\\sub", "Move")).toMatch(/itself/);
    expect(dropRejection(["C:\\a"], "C:\\a", "Copy")).toMatch(/itself/);
    expect(dropRejection(["C:\\a\\x", "C:\\a\\y"], "C:\\a", "Move")).toMatch(/Already/);
    expect(dropRejection(["C:\\a\\x"], "C:\\a", "Copy")).toBeNull(); // copy-here is a duplicate
    expect(dropRejection(["C:\\a\\x"], "C:\\b", "Move")).toBeNull();
  });

  it("round-trips the payload and ignores foreign data", () => {
    const p = { paths: ["C:\\a"], sourceDir: "C:\\" };
    expect(decodePayload(encodePayload(p))).toEqual(p);
    expect(decodePayload("not json")).toBeNull();
    expect(decodePayload(JSON.stringify({ paths: [1] }))).toBeNull();
  });
});

describe("type-ahead", () => {
  const names = ["alpha", "beta", "bravo", "charlie", "Banana"];
  const at = (i: number) => names[i]!;

  it("jumps to the first prefix match after the current row", () => {
    const s = feed(EMPTY_TYPEAHEAD, "b", 1000);
    expect(findMatch(s.buffer, names.length, at, 0)).toBe(1);
  });

  it("extends the prefix while typing quickly", () => {
    let s = feed(EMPTY_TYPEAHEAD, "b", 1000);
    s = feed(s, "r", 1100);
    expect(s.buffer).toBe("br");
    expect(findMatch(s.buffer, names.length, at, 1)).toBe(2);
  });

  it("cycles through same-letter matches on a repeated key", () => {
    let s = feed(EMPTY_TYPEAHEAD, "b", 1000);
    s = feed(s, "b", 1100);
    expect(findMatch(s.buffer, names.length, at, 1)).toBe(2);
    expect(findMatch(s.buffer, names.length, at, 2)).toBe(4); // case-insensitive
    expect(findMatch(s.buffer, names.length, at, 4)).toBe(1); // wraps
  });

  it("starts a new prefix after a pause", () => {
    let s = feed(EMPTY_TYPEAHEAD, "b", 1000);
    s = feed(s, "c", 1000 + TYPEAHEAD_RESET_MS + 1);
    expect(s.buffer).toBe("c");
  });

  it("returns null when nothing matches", () => {
    expect(findMatch("zz", names.length, at, 0)).toBeNull();
  });
});

describe("file types", () => {
  it("labels by extension with sensible fallbacks", () => {
    expect(typeInfo({ kind: "Dir", ext: null }).label).toBe("Folder");
    expect(typeInfo({ kind: "File", ext: "png" })).toEqual({ icon: "image", label: "PNG image" });
    expect(typeInfo({ kind: "File", ext: "qqq" }).label).toBe("QQQ file");
    expect(typeInfo({ kind: "File", ext: null }).label).toBe("File");
  });

  it("decides preview capability without touching the file", () => {
    expect(previewKind({ kind: "File", ext: "jpg" })).toBe("image");
    expect(previewKind({ kind: "File", ext: "md" })).toBe("text");
    expect(previewKind({ kind: "File", ext: "pdf" })).toBe("pdf");
    expect(previewKind({ kind: "File", ext: "mp4" })).toBe("media");
    expect(previewKind({ kind: "Dir", ext: null })).toBe("none");
  });
});
