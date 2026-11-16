/**
 * Column model for the details view.
 *
 * Widths are applied as CSS custom properties on the table root rather than as props on
 * each cell, so dragging a column edge rewrites one style attribute instead of
 * re-rendering every visible row sixty times a second.
 */
import type { SortKey } from "../api/types";

export type ColumnId = "name" | "size" | "type" | "modified" | "created" | "path";

export interface ColumnDef {
  id: ColumnId;
  label: string;
  /** Rust-side sort key this column requests when its header is clicked. */
  sortKey: SortKey;
  align: "left" | "right";
  minWidth: number;
  defaultWidth: number;
  /** Numeric columns get tabular figures so digits do not jitter while scrolling. */
  numeric: boolean;
}

export const COLUMNS: Record<ColumnId, ColumnDef> = {
  name: { id: "name", label: "Name", sortKey: "Name", align: "left", minWidth: 140, defaultWidth: 360, numeric: false },
  size: { id: "size", label: "Size", sortKey: "Size", align: "right", minWidth: 64, defaultWidth: 96, numeric: true },
  type: { id: "type", label: "Type", sortKey: "Extension", align: "left", minWidth: 72, defaultWidth: 140, numeric: false },
  modified: { id: "modified", label: "Modified", sortKey: "Modified", align: "right", minWidth: 96, defaultWidth: 150, numeric: true },
  created: { id: "created", label: "Created", sortKey: "Created", align: "right", minWidth: 96, defaultWidth: 150, numeric: true },
  path: { id: "path", label: "Folder", sortKey: "Path", align: "left", minWidth: 120, defaultWidth: 320, numeric: false },
};

export const LISTING_COLUMNS: ColumnId[] = ["name", "size", "type", "modified", "created"];
export const SEARCH_COLUMNS: ColumnId[] = ["name", "path", "size", "modified"];

export interface ColumnLayout {
  order: ColumnId[];
  widths: Partial<Record<ColumnId, number>>;
}

export function widthOf(layout: ColumnLayout, id: ColumnId): number {
  return layout.widths[id] ?? COLUMNS[id].defaultWidth;
}

export function resizeColumn(layout: ColumnLayout, id: ColumnId, width: number): ColumnLayout {
  const w = Math.round(Math.max(COLUMNS[id].minWidth, Math.min(width, 2000)));
  return { ...layout, widths: { ...layout.widths, [id]: w } };
}

/** Moves the column at `from` so it ends up at `to` (an index in the *resulting* order). */
export function moveColumn(order: readonly ColumnId[], from: number, to: number): ColumnId[] {
  const out = [...order];
  if (from < 0 || from >= out.length) return out;
  const [col] = out.splice(from, 1);
  const dest = Math.max(0, Math.min(to, out.length));
  out.splice(dest, 0, col!);
  return out;
}

/**
 * Which insertion slot a header drag at `x` points to: the boundary nearest the pointer,
 * computed from the widths rather than the DOM so it works mid-drag without layout reads.
 */
export function dropSlot(widths: readonly number[], x: number): number {
  let edge = 0;
  for (let i = 0; i < widths.length; i++) {
    const mid = edge + widths[i]! / 2;
    if (x < mid) return i;
    edge += widths[i]!;
  }
  return widths.length;
}

/** Converts an insertion slot into the `to` argument of `moveColumn`. */
export function slotToIndex(from: number, slot: number): number {
  return slot > from ? slot - 1 : slot;
}

/** Total width, so the table can scroll horizontally when columns overflow the pane. */
export function totalWidth(layout: ColumnLayout): number {
  return layout.order.reduce((sum, id) => sum + widthOf(layout, id), 0);
}

/** CSS custom properties for every column width. */
export function columnVars(layout: ColumnLayout): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const id of Object.keys(COLUMNS) as ColumnId[]) vars[`--col-${id}`] = `${widthOf(layout, id)}px`;
  return vars;
}
