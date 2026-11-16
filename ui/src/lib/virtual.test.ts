import { describe, expect, it } from "vitest";
import {
  MAX_SCROLL_PX,
  cellsInRect,
  edgeScrollSpeed,
  gridColumns,
  gridWindow,
  moveFocus,
  rectFrom,
  rowsInRect,
  scaledWindow,
  scrollModel,
  scrollToIndex,
  selectionRange,
  toPhysical,
  windowFor,
} from "./virtual";

const ROW = 28;
const VIEW = 560; // 20 rows

describe("windowFor", () => {
  it("renders only the visible slice plus overscan", () => {
    const w = windowFor(0, VIEW, ROW, 1_000_000, 8);
    expect(w.start).toBe(0);
    // 20 visible + 8 overscan below.
    expect(w.end).toBe(28);
    expect(w.totalHeight).toBe(1_000_000 * ROW);
  });

  it("keeps the rendered count bounded no matter how far down we scroll", () => {
    const deep = windowFor(500_000 * ROW, VIEW, ROW, 1_000_000, 8);
    expect(deep.end - deep.start).toBeLessThanOrEqual(20 + 16 + 1);
    expect(deep.offsetY).toBe(deep.start * ROW);
  });

  it("clamps overscroll instead of producing a negative window", () => {
    const above = windowFor(-500, VIEW, ROW, 1000);
    expect(above.start).toBe(0);
    expect(above.offsetY).toBe(0);

    // A stale scrollTop larger than the content must not read past the end.
    const past = windowFor(99_999_999, VIEW, ROW, 1000);
    expect(past.end).toBe(1000);
    expect(past.start).toBeGreaterThanOrEqual(0);
  });

  it("returns an empty window for an empty list", () => {
    const w = windowFor(0, VIEW, ROW, 0);
    expect(w).toEqual({ start: 0, end: 0, offsetY: 0, totalHeight: 0 });
  });

  it("does not divide by zero when the viewport has not been measured yet", () => {
    expect(windowFor(0, 0, ROW, 500).end).toBe(0);
    expect(windowFor(0, VIEW, 0, 500).end).toBe(0);
  });
});

describe("scrollToIndex", () => {
  it("does nothing when the row is already visible", () => {
    expect(scrollToIndex(5, 0, VIEW, ROW)).toBeNull();
  });

  it("scrolls up to a row above the viewport", () => {
    expect(scrollToIndex(2, 500, VIEW, ROW)).toBe(2 * ROW);
  });

  it("scrolls down just enough to reveal a row below the viewport", () => {
    // Row 30 ends at 31*28 = 868; viewport is 560 tall, so top must be 308.
    expect(scrollToIndex(30, 0, VIEW, ROW)).toBe(31 * ROW - VIEW);
  });
});

describe("selectionRange", () => {
  it("is inclusive and direction-agnostic", () => {
    expect(selectionRange(3, 7)).toEqual([3, 7]);
    expect(selectionRange(7, 3)).toEqual([3, 7]);
    expect(selectionRange(4, 4)).toEqual([4, 4]);
  });
});


describe("scaledWindow", () => {
  it("is the identity below the browser height limit", () => {
    const w = scaledWindow(2800, VIEW, ROW, 10_000);
    const plain = windowFor(2800, VIEW, ROW, 10_000);
    expect(w.start).toBe(plain.start);
    expect(w.offsetY).toBe(plain.offsetY);
    expect(w.totalHeight).toBe(10_000 * ROW);
    expect(w.model.ratio).toBe(1);
  });

  it("caps the spacer for a million rows at comfortable density", () => {
    const rows = 1_000_000;
    const row = 34; // 34M px would exceed Chromium's ~33.5M element limit
    const top = scaledWindow(0, VIEW, row, rows);
    expect(top.totalHeight).toBe(MAX_SCROLL_PX);
    expect(top.start).toBe(0);

    // Scrolled to the physical bottom, the very last row must be rendered and aligned
    // with the bottom of the viewport.
    const bottom = scaledWindow(MAX_SCROLL_PX - VIEW, VIEW, row, rows);
    expect(bottom.end).toBe(rows);
    const lastRowTop = bottom.offsetY + (rows - 1 - bottom.start) * row;
    expect(lastRowTop + row).toBeCloseTo(MAX_SCROLL_PX, 3);
  });

  it("maps a physical position to the proportional row", () => {
    const rows = 1_000_000;
    const m = scrollModel(rows * ROW, VIEW);
    const half = toPhysical(m, (rows * ROW - VIEW) / 2);
    const w = scaledWindow(half, VIEW, ROW, rows, 0);
    expect(w.start).toBeGreaterThan(rows / 2 - 30);
    expect(w.start).toBeLessThan(rows / 2 + 30);
    // The first visible row is drawn at or just above the viewport top.
    const firstTop = w.offsetY;
    expect(firstTop).toBeLessThanOrEqual(half);
    expect(firstTop).toBeGreaterThan(half - ROW);
  });
});

describe("grid maths", () => {
  it("fits at least one column", () => {
    expect(gridColumns(500, 120)).toBe(4);
    expect(gridColumns(50, 120)).toBe(1);
    expect(gridColumns(0, 120)).toBe(1);
  });

  it("renders whole rows of cells", () => {
    const g = gridWindow(0, 400, 480, 120, 100, 1000, 1);
    expect(g.columns).toBe(4);
    expect(g.rows).toBe(250);
    expect(g.start).toBe(0);
    expect(g.end).toBe((4 + 1) * 4); // 4 visible rows + 1 overscan
  });

  it("clamps the last partial row", () => {
    const g = gridWindow(1e9, 400, 480, 120, 100, 10, 1);
    expect(g.end).toBe(10);
  });
});

describe("moveFocus", () => {
  it("moves in a list", () => {
    expect(moveFocus(null, "down", 10, 1, 5)).toBe(0);
    expect(moveFocus(null, "end", 10, 1, 5)).toBe(9);
    expect(moveFocus(3, "up", 10, 1, 5)).toBe(2);
    expect(moveFocus(0, "up", 10, 1, 5)).toBe(0);
    expect(moveFocus(9, "down", 10, 1, 5)).toBe(9);
    expect(moveFocus(3, "pageDown", 10, 1, 5)).toBe(8);
    expect(moveFocus(8, "pageDown", 10, 1, 5)).toBe(9);
    expect(moveFocus(3, "left", 10, 1, 5)).toBe(3);
    expect(moveFocus(3, "home", 10, 1, 5)).toBe(0);
  });

  it("moves in a grid", () => {
    // 4 columns, 10 items: rows [0-3] [4-7] [8-9]
    expect(moveFocus(5, "up", 10, 4, 2)).toBe(1);
    expect(moveFocus(5, "down", 10, 4, 2)).toBe(9); // short last row → last item
    expect(moveFocus(9, "down", 10, 4, 2)).toBe(9);
    expect(moveFocus(5, "right", 10, 4, 2)).toBe(6);
    expect(moveFocus(0, "left", 10, 4, 2)).toBe(0);
    expect(moveFocus(1, "pageDown", 10, 4, 2)).toBe(9);
  });

  it("returns null for an empty list", () => {
    expect(moveFocus(null, "down", 0, 1, 5)).toBeNull();
  });
});

describe("marquee", () => {
  it("normalises drags from any corner", () => {
    expect(rectFrom(10, 50, 0, 5)).toEqual({ x0: 0, y0: 5, x1: 10, y1: 50 });
  });

  it("finds rows by maths, including rows not in the DOM", () => {
    expect(rowsInRect(rectFrom(0, 30, 100, 29_000), ROW, 1_000_000)).toEqual([1, 1035]);
    expect(rowsInRect(rectFrom(0, 0, 10, 10), ROW, 0)).toBeNull();
    expect(rowsInRect(rectFrom(0, 5000, 10, 6000), ROW, 10)).toBeNull();
  });

  it("finds grid cells as one range per row", () => {
    // 4 cols of 100x100, 10 items. Rect covers cols 1-2, rows 0-2.
    const r = cellsInRect(rectFrom(150, 50, 250, 250), 4, 100, 100, 10);
    expect(r).toEqual([
      [1, 2],
      [5, 6],
      [9, 9],
    ]);
  });

  it("ramps edge auto-scroll speed", () => {
    expect(edgeScrollSpeed(300, 0, 600)).toBe(0);
    expect(edgeScrollSpeed(0, 0, 600)).toBe(-28);
    expect(edgeScrollSpeed(600, 0, 600)).toBe(28);
    expect(edgeScrollSpeed(580, 0, 600)).toBe(14);
  });
});
