import { describe, expect, it } from "vitest";
import { fromIndices, merge, Selection } from "./selection";

const E = Selection.EMPTY;

describe("Selection", () => {
  it("click selects one row and sets the anchor", () => {
    const s = E.select(5);
    expect(s.ranges).toEqual([[5, 5]]);
    expect(s.anchor).toBe(5);
    expect(s.has(5)).toBe(true);
    expect(s.has(4)).toBe(false);
  });

  it("ctrl-click toggles without disturbing other rows", () => {
    const s = E.select(2).toggle(4).toggle(3);
    expect(s.ranges).toEqual([[2, 4]]);
    const t = s.toggle(3);
    expect(t.ranges).toEqual([
      [2, 2],
      [4, 4],
    ]);
    expect(t.size).toBe(2);
  });

  it("shift-click replaces with the anchor range in either direction", () => {
    const s = E.select(10).extendTo(14);
    expect(s.ranges).toEqual([[10, 14]]);
    const back = s.extendTo(7);
    expect(back.ranges).toEqual([[7, 10]]);
    expect(back.anchor).toBe(10);
    expect(back.focus).toBe(7);
  });

  it("ctrl+shift-click adds a range to the existing selection", () => {
    const s = E.select(1).toggle(10).addRange(12);
    expect(s.ranges).toEqual([
      [1, 1],
      [10, 12],
    ]);
  });

  it("select-all is constant size at a million rows", () => {
    const s = E.all(1_000_000);
    expect(s.ranges.length).toBe(1);
    expect(s.size).toBe(1_000_000);
    expect(s.has(999_999)).toBe(true);
    expect(s.has(1_000_000)).toBe(false);
    // Ctrl-click a hole into it: still two ranges, not 999,999 entries.
    expect(s.toggle(500_000).ranges.length).toBe(2);
  });

  it("clamps after a listing shrinks", () => {
    const s = E.select(5).extendTo(20).clamp(10);
    expect(s.ranges).toEqual([[5, 9]]);
    expect(s.focus).toBe(9);
    expect(E.select(3).clamp(0)).toBe(Selection.EMPTY);
  });

  it("has() agrees with a brute-force membership check", () => {
    let s = E;
    const truth = new Set<number>();
    let seed = 7;
    const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) % 200;
    for (let k = 0; k < 300; k++) {
      const i = rnd();
      s = s.toggle(i);
      if (truth.has(i)) truth.delete(i);
      else truth.add(i);
    }
    for (let i = 0; i < 200; i++) expect(s.has(i)).toBe(truth.has(i));
    expect(s.size).toBe(truth.size);
    expect([...s.indices()]).toEqual([...truth].sort((a, b) => a - b));
  });

  it("inverts without enumerating rows", () => {
    const s = E.select(0).toggle(5).toggle(6).invert(10);
    expect(s.ranges).toEqual([
      [1, 4],
      [7, 9],
    ]);
    expect(E.invert(1_000_000).size).toBe(1_000_000);
    expect(E.all(3).invert(3).isEmpty).toBe(true);
  });

  it("union merges a marquee into an existing selection", () => {
    const s = E.select(0).union([
      [3, 6],
      [5, 9],
    ]);
    expect(s.ranges).toEqual([
      [0, 0],
      [3, 9],
    ]);
  });
});

describe("merge and fromIndices", () => {
  it("merges overlapping and adjacent ranges", () => {
    expect(
      merge([
        [5, 7],
        [1, 2],
        [3, 4],
        [9, 8],
      ]),
    ).toEqual([
      [1, 9],
    ]);
  });

  it("rebuilds from scattered indices", () => {
    const s = fromIndices([9, 1, 2, 3, 7]);
    expect(s.ranges).toEqual([
      [1, 3],
      [7, 7],
      [9, 9],
    ]);
    expect(s.focus).toBe(1);
  });
});
