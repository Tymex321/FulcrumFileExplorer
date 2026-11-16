/**
 * Virtual-list window maths.
 *
 * Pure functions, kept separate from React so they can be tested directly — this is the
 * code that has to stay correct at a million rows, where an off-by-one in the overscan
 * shows up as a flash of blank rows during fast scrolling.
 */

export interface Window {
  /** First row to render, including overscan. */
  start: number;
  /** One past the last row to render. */
  end: number;
  /** Pixel offset to translate the rendered block by. */
  offsetY: number;
  /** Full scrollable height, for the spacer element. */
  totalHeight: number;
}

/**
 * Computes which rows to render for a scroll position.
 *
 * `overscan` rows are drawn beyond each edge so that a fast flick does not expose empty
 * space before the next frame lands.
 */
export function windowFor(
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  rowCount: number,
  overscan = 8,
): Window {
  const totalHeight = rowCount * rowHeight;

  if (rowCount === 0 || rowHeight <= 0 || viewportHeight <= 0) {
    return { start: 0, end: 0, offsetY: 0, totalHeight: Math.max(0, totalHeight) };
  }

  // Clamp so overscrolling (rubber-banding, or a stale scrollTop after the row count
  // shrinks) cannot produce a negative or out-of-range window.
  const maxScroll = Math.max(0, totalHeight - viewportHeight);
  const clamped = Math.min(Math.max(scrollTop, 0), maxScroll);

  const firstVisible = Math.floor(clamped / rowHeight);
  const visibleCount = Math.ceil(viewportHeight / rowHeight);

  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(rowCount, firstVisible + visibleCount + overscan);

  return { start, end, offsetY: start * rowHeight, totalHeight };
}

/**
 * Scroll offset that brings `index` into view, moving as little as possible.
 *
 * Returns `null` when the row is already fully visible, so callers can skip a pointless
 * scroll write that would interrupt smooth scrolling.
 */
export function scrollToIndex(
  index: number,
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
): number | null {
  const top = index * rowHeight;
  const bottom = top + rowHeight;

  if (top < scrollTop) return top;
  if (bottom > scrollTop + viewportHeight) return bottom - viewportHeight;
  return null;
}

/**
 * Expands a shift-click selection into an inclusive index range, in either direction.
 */
export function selectionRange(anchor: number, focus: number): [number, number] {
  return anchor <= focus ? [anchor, focus] : [focus, anchor];
}

/* ───────────────────────── Scroll scaling for very large lists ─────────────────────────

   Browsers cap how tall an element may be: roughly 33.5M px in Chromium (WebView2),
   ~17.9M in Firefox. A million rows at the comfortable density is 34M px, so a naive
   spacer silently truncates and the last rows become unreachable. Past `MAX_SCROLL_PX`
   the spacer is capped and the scroll position is mapped proportionally onto the real
   ("virtual") content height. Below it the mapping is the identity, so ordinary
   directories scroll pixel-for-pixel. */

/** Comfortably under every engine's limit, with headroom for the grid views. */
export const MAX_SCROLL_PX = 8_000_000;

export interface ScrollModel {
  /** Height of the spacer actually placed in the DOM. */
  physicalHeight: number;
  /** Full content height the rows would need unscaled. */
  virtualHeight: number;
  /** Virtual pixels per physical pixel; exactly 1 when no scaling is needed. */
  ratio: number;
}

export function scrollModel(virtualHeight: number, viewport: number, max = MAX_SCROLL_PX): ScrollModel {
  if (virtualHeight <= max || viewport >= max) {
    return { physicalHeight: virtualHeight, virtualHeight, ratio: 1 };
  }
  const ratio = Math.max(1, (virtualHeight - viewport) / (max - viewport));
  return { physicalHeight: max, virtualHeight, ratio };
}

export const toVirtual = (m: ScrollModel, physicalTop: number) => physicalTop * m.ratio;
export const toPhysical = (m: ScrollModel, virtualTop: number) => virtualTop / m.ratio;

export interface ScaledWindow extends Window {
  /** Content-space position of the viewport's top edge. */
  virtualTop: number;
  model: ScrollModel;
}

/**
 * `windowFor`, but correct past the browser's element-height limit. `offsetY` is in
 * physical (DOM) pixels, ready for the transform; everything else is in rows.
 */
export function scaledWindow(
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  rowCount: number,
  overscan = 8,
  max = MAX_SCROLL_PX,
): ScaledWindow {
  const model = scrollModel(rowCount * rowHeight, viewportHeight, max);
  const virtualTop = toVirtual(model, scrollTop);
  const w = windowFor(virtualTop, viewportHeight, rowHeight, rowCount, overscan);
  // In scaled mode the block is positioned relative to the live scroll offset, so rows
  // stay pixel-aligned with the viewport even though the spacer is compressed.
  const offsetY = model.ratio === 1 ? w.offsetY : scrollTop + (w.offsetY - virtualTop);
  return { ...w, offsetY, totalHeight: model.physicalHeight, virtualTop, model };
}

/* ───────────────────────────────── Grid layout ───────────────────────────────── */

export interface GridWindow extends Window {
  columns: number;
  rows: number;
  /** First and one-past-last grid row rendered. */
  startRow: number;
  endRow: number;
  virtualTop: number;
  model: ScrollModel;
}

/** How many cells fit across, never fewer than one so a narrow pane still works. */
export function gridColumns(width: number, cellWidth: number): number {
  if (cellWidth <= 0 || width <= 0) return 1;
  return Math.max(1, Math.floor(width / cellWidth));
}

/** The grid views are a virtual list of *rows of cells*; this reuses the same maths. */
export function gridWindow(
  scrollTop: number,
  viewportHeight: number,
  viewportWidth: number,
  cellWidth: number,
  cellHeight: number,
  count: number,
  overscanRows = 2,
): GridWindow {
  const columns = gridColumns(viewportWidth, cellWidth);
  const rows = Math.ceil(count / columns);
  const w = scaledWindow(scrollTop, viewportHeight, cellHeight, rows, overscanRows);
  return {
    ...w,
    columns,
    rows,
    startRow: w.start,
    endRow: w.end,
    start: w.start * columns,
    end: Math.min(count, w.end * columns),
  };
}

/* ─────────────────────────────── Keyboard movement ─────────────────────────────── */

export type Move = "up" | "down" | "left" | "right" | "pageUp" | "pageDown" | "home" | "end";

/**
 * Where the cursor goes for a navigation key. `columns` is 1 in the details view, where
 * left/right do not move between rows (they belong to horizontal scrolling).
 */
export function moveFocus(
  current: number | null,
  move: Move,
  count: number,
  columns: number,
  pageRows: number,
): number | null {
  if (count <= 0) return null;
  const last = count - 1;
  // With nothing focused yet, any movement lands on the first row — Explorer's behaviour.
  if (current === null) return move === "end" ? last : 0;
  const cols = Math.max(1, columns);
  const page = Math.max(1, pageRows) * cols;
  const clamp = (i: number) => Math.min(last, Math.max(0, i));
  switch (move) {
    case "up":
      return current - cols < 0 ? current : current - cols;
    case "down":
      // In a grid, moving down from a short last row lands on the final item.
      return current + cols > last ? (Math.floor(current / cols) < Math.floor(last / cols) ? last : current) : current + cols;
    case "left":
      return cols === 1 ? current : clamp(current - 1);
    case "right":
      return cols === 1 ? current : clamp(current + 1);
    case "pageUp":
      return clamp(current - page);
    case "pageDown":
      return clamp(current + page);
    case "home":
      return 0;
    case "end":
      return last;
  }
}

/* ─────────────────────────────── Marquee hit-testing ─────────────────────────────── */

export interface Rect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** Normalises a drag from any corner into a rect with x0 ≤ x1 and y0 ≤ y1. */
export function rectFrom(ax: number, ay: number, bx: number, by: number): Rect {
  return { x0: Math.min(ax, bx), y0: Math.min(ay, by), x1: Math.max(ax, bx), y1: Math.max(ay, by) };
}

/**
 * Rows a marquee touches in a single-column list, in content coordinates. Pure maths
 * rather than DOM hit-testing, because most of the rows it covers are not in the DOM.
 */
export function rowsInRect(rect: Rect, rowHeight: number, count: number): [number, number] | null {
  if (count <= 0 || rowHeight <= 0 || rect.y1 < 0) return null;
  const first = Math.max(0, Math.floor(rect.y0 / rowHeight));
  const last = Math.min(count - 1, Math.floor(rect.y1 / rowHeight));
  return first > last ? null : [first, last];
}

/**
 * Cells a marquee touches in a grid, as one index range per grid row. A cell counts when
 * the rectangle overlaps it at all, matching Explorer.
 */
export function cellsInRect(
  rect: Rect,
  columns: number,
  cellWidth: number,
  cellHeight: number,
  count: number,
): [number, number][] {
  if (count <= 0 || cellWidth <= 0 || cellHeight <= 0) return [];
  const c0 = Math.max(0, Math.floor(rect.x0 / cellWidth));
  const c1 = Math.min(columns - 1, Math.floor(rect.x1 / cellWidth));
  if (c0 > c1) return [];
  const rows = Math.ceil(count / columns);
  const r0 = Math.max(0, Math.floor(rect.y0 / cellHeight));
  const r1 = Math.min(rows - 1, Math.floor(rect.y1 / cellHeight));
  const out: [number, number][] = [];
  for (let r = r0; r <= r1; r++) {
    const a = r * columns + c0;
    const b = Math.min(count - 1, r * columns + c1);
    if (a <= b) out.push([a, b]);
  }
  return out;
}

/**
 * Auto-scroll speed while a marquee or drag hovers near an edge: zero outside the
 * `zone`, ramping to `max` px/frame at the edge itself.
 */
export function edgeScrollSpeed(pointerY: number, top: number, bottom: number, zone = 40, max = 28): number {
  if (pointerY < top + zone) return -Math.round(max * Math.min(1, (top + zone - pointerY) / zone));
  if (pointerY > bottom - zone) return Math.round(max * Math.min(1, (pointerY - (bottom - zone)) / zone));
  return 0;
}
