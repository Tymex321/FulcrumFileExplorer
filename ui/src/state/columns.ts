/**
 * Column order and widths, persisted to localStorage.
 *
 * Kept out of `Settings` deliberately: it is presentation state that changes on every
 * drag of a column edge, and round-tripping it through the Rust settings file on each
 * change would be pointless IPC.
 */
import { type ColumnId, type ColumnLayout, LISTING_COLUMNS, SEARCH_COLUMNS } from "../lib/columns";
import { createStore, useStore } from "./store";

export interface ColumnState {
  list: ColumnLayout;
  search: ColumnLayout;
}

const KEY = "fulcrum.columns.v1";

function load(): ColumnState {
  const fallback: ColumnState = {
    list: { order: [...LISTING_COLUMNS], widths: {} },
    search: { order: [...SEARCH_COLUMNS], widths: {} },
  };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return fallback;
    const v = JSON.parse(raw) as ColumnState;
    // Drop unknown ids and restore any missing ones, so an old layout cannot hide a column.
    const repair = (l: ColumnLayout | undefined, all: ColumnId[]): ColumnLayout => {
      const order = (l?.order ?? []).filter((c) => all.includes(c));
      for (const c of all) if (!order.includes(c)) order.push(c);
      return { order, widths: l?.widths ?? {} };
    };
    return { list: repair(v.list, LISTING_COLUMNS), search: repair(v.search, SEARCH_COLUMNS) };
  } catch {
    return fallback;
  }
}

export const columns = createStore<ColumnState>(load());
columns.subscribe(() => {
  try {
    localStorage.setItem(KEY, JSON.stringify(columns.get()));
  } catch {
    /* quota or private mode: layout just will not persist */
  }
});

export const useColumns = <S>(sel: (c: ColumnState) => S) => useStore(columns, sel);

export function setLayout(kind: keyof ColumnState, layout: ColumnLayout): void {
  columns.set((c) => ({ ...c, [kind]: layout }));
}
