/**
 * Tabs, panes and per-tab history as a pure reducer.
 *
 * Everything that changes *where* the user is goes through these functions, which makes
 * the tricky cases — closing the last tab in a split, restoring a tab into a pane that no
 * longer exists, a corrupt session file — testable without rendering anything.
 */
import type { PaneLayout, SessionState, Sort, TabState, ViewMode } from "../api/types";
import { normalize } from "./path";

export interface Tab {
  /** Stable identity for React keys and the per-tab listing store; not persisted. */
  id: string;
  path: string;
  view: ViewMode;
  sort: Sort;
  history: string[];
  historyPos: number;
}

export interface PaneState {
  tabs: Tab[];
  active: number;
}

export interface ClosedTab {
  tab: Tab;
  pane: 0 | 1;
  index: number;
}

export interface Workspace {
  layout: PaneLayout;
  /** Always two panes; the second is simply hidden in the single layout, so toggling a
      split off and on again brings its tabs back instead of discarding them. */
  panes: [PaneState, PaneState];
  activePane: 0 | 1;
  closed: ClosedTab[];
}

export const HISTORY_LIMIT = 100;
export const CLOSED_LIMIT = 25;
export const DEFAULT_SORT: Sort = { key: "Name", desc: false, dirs_first: true };

let seq = 0;
export function newTabId(): string {
  seq += 1;
  return `t${seq.toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

export function makeTab(path: string, view: ViewMode = "Details", sort: Sort = DEFAULT_SORT): Tab {
  const p = normalize(path);
  return { id: newTabId(), path: p, view, sort, history: [p], historyPos: 0 };
}

export function initialWorkspace(path: string, view: ViewMode = "Details"): Workspace {
  return {
    layout: "Single",
    panes: [
      { tabs: [makeTab(path, view)], active: 0 },
      { tabs: [], active: 0 },
    ],
    activePane: 0,
    closed: [],
  };
}

export const isSplit = (ws: Workspace) => ws.layout !== "Single";

export function activeTab(ws: Workspace, pane: 0 | 1 = ws.activePane): Tab | undefined {
  const p = ws.panes[pane];
  return p.tabs[p.active];
}

function withPane(ws: Workspace, pane: 0 | 1, next: PaneState): Workspace {
  const panes: [PaneState, PaneState] = pane === 0 ? [next, ws.panes[1]] : [ws.panes[0], next];
  return { ...ws, panes };
}

function updateTab(ws: Workspace, pane: 0 | 1, index: number, fn: (t: Tab) => Tab): Workspace {
  const p = ws.panes[pane];
  const tab = p.tabs[index];
  if (!tab) return ws;
  const tabs = p.tabs.slice();
  tabs[index] = fn(tab);
  return withPane(ws, pane, { ...p, tabs });
}

export function openTab(
  ws: Workspace,
  pane: 0 | 1,
  path: string,
  opts: { activate?: boolean; view?: ViewMode } = {},
): Workspace {
  const p = ws.panes[pane];
  const current = p.tabs[p.active];
  const tab = makeTab(path, opts.view ?? current?.view ?? "Details", current?.sort ?? DEFAULT_SORT);
  // New tabs open next to the current one, as in every browser, not at the far end.
  const at = p.tabs.length === 0 ? 0 : p.active + 1;
  const tabs = [...p.tabs.slice(0, at), tab, ...p.tabs.slice(at)];
  const activate = opts.activate ?? true;
  const next = withPane(ws, pane, { tabs, active: activate ? at : p.active + (at <= p.active ? 1 : 0) });
  return activate ? { ...next, activePane: pane } : next;
}

export function closeTab(ws: Workspace, pane: 0 | 1, index: number): Workspace {
  const p = ws.panes[pane];
  const tab = p.tabs[index];
  if (!tab) return ws;

  const closed = [{ tab, pane, index }, ...ws.closed].slice(0, CLOSED_LIMIT);

  if (p.tabs.length === 1) {
    // Closing the last tab of a split collapses to the other pane; the last tab of the
    // whole window stays, since a file manager with no folder open is not a state.
    if (!isSplit(ws)) return ws;
    const other = ws.panes[pane === 0 ? 1 : 0];
    return {
      layout: "Single",
      panes: [other, { tabs: [], active: 0 }],
      activePane: 0,
      closed,
    };
  }

  const tabs = p.tabs.filter((_, i) => i !== index);
  // Closing the active tab activates its right neighbour (or the new last tab).
  let active = p.active;
  if (index < p.active) active -= 1;
  else if (index === p.active) active = Math.min(index, tabs.length - 1);
  return { ...withPane(ws, pane, { tabs, active }), closed };
}

export function restoreClosed(ws: Workspace): Workspace {
  const [entry, ...rest] = ws.closed;
  if (!entry) return ws;
  // Fall back to the active pane if the original one is hidden now.
  const pane: 0 | 1 = entry.pane === 1 && !isSplit(ws) ? 0 : entry.pane;
  const p = ws.panes[pane];
  const at = Math.min(entry.index, p.tabs.length);
  const tab = { ...entry.tab, id: newTabId() };
  const tabs = [...p.tabs.slice(0, at), tab, ...p.tabs.slice(at)];
  return { ...withPane(ws, pane, { tabs, active: at }), activePane: pane, closed: rest };
}

export function activateTab(ws: Workspace, pane: 0 | 1, index: number): Workspace {
  const p = ws.panes[pane];
  if (index < 0 || index >= p.tabs.length) return ws;
  return { ...withPane(ws, pane, { ...p, active: index }), activePane: pane };
}

/** Ctrl+Tab / Ctrl+Shift+Tab, wrapping. */
export function cycleTab(ws: Workspace, delta: number): Workspace {
  const p = ws.panes[ws.activePane];
  if (p.tabs.length < 2) return ws;
  const n = p.tabs.length;
  return activateTab(ws, ws.activePane, (((p.active + delta) % n) + n) % n);
}

export function moveTab(ws: Workspace, pane: 0 | 1, from: number, to: number): Workspace {
  const p = ws.panes[pane];
  if (from === to || from < 0 || from >= p.tabs.length) return ws;
  const tabs = p.tabs.slice();
  const [tab] = tabs.splice(from, 1);
  const dest = Math.max(0, Math.min(to, tabs.length));
  tabs.splice(dest, 0, tab!);
  // The active tab keeps its identity through the move, not its old index.
  const activeId = p.tabs[p.active]?.id;
  const active = Math.max(0, tabs.findIndex((t) => t.id === activeId));
  return withPane(ws, pane, { tabs, active });
}

/** Drags a tab into the other pane. */
export function moveTabToPane(ws: Workspace, from: 0 | 1, index: number, to: 0 | 1, toIndex: number): Workspace {
  if (from === to) return moveTab(ws, from, index, toIndex);
  const src = ws.panes[from];
  const tab = src.tabs[index];
  if (!tab || src.tabs.length === 1) return ws; // never leave a visible pane empty
  const srcTabs = src.tabs.filter((_, i) => i !== index);
  const srcActive = Math.min(index < src.active ? src.active - 1 : src.active, srcTabs.length - 1);
  const dst = ws.panes[to];
  const at = Math.max(0, Math.min(toIndex, dst.tabs.length));
  const dstTabs = [...dst.tabs.slice(0, at), tab, ...dst.tabs.slice(at)];
  let next = withPane(ws, from, { tabs: srcTabs, active: srcActive });
  next = withPane(next, to, { tabs: dstTabs, active: at });
  return { ...next, activePane: to };
}

/** Goes to `path` in the active tab of `pane`, discarding any forward history. */
export function navigate(ws: Workspace, pane: 0 | 1, path: string): Workspace {
  const p = normalize(path);
  const index = ws.panes[pane].active;
  return updateTab(ws, pane, index, (t) => {
    if (normalize(t.path) === p) return t;
    const history = [...t.history.slice(0, t.historyPos + 1), p].slice(-HISTORY_LIMIT);
    return { ...t, path: p, history, historyPos: history.length - 1 };
  });
}

export function canGoBack(t: Tab | undefined) {
  return !!t && t.historyPos > 0;
}
export function canGoForward(t: Tab | undefined) {
  return !!t && t.historyPos < t.history.length - 1;
}

function stepHistory(ws: Workspace, pane: 0 | 1, delta: number): Workspace {
  const index = ws.panes[pane].active;
  return updateTab(ws, pane, index, (t) => {
    const pos = t.historyPos + delta;
    const path = t.history[pos];
    if (path === undefined) return t;
    return { ...t, path, historyPos: pos };
  });
}

export const back = (ws: Workspace, pane: 0 | 1) => stepHistory(ws, pane, -1);
export const forward = (ws: Workspace, pane: 0 | 1) => stepHistory(ws, pane, 1);

export function setView(ws: Workspace, pane: 0 | 1, view: ViewMode): Workspace {
  return updateTab(ws, pane, ws.panes[pane].active, (t) => ({ ...t, view }));
}

export function setSort(ws: Workspace, pane: 0 | 1, sort: Sort): Workspace {
  return updateTab(ws, pane, ws.panes[pane].active, (t) => ({ ...t, sort }));
}

/** Clicking a sorted column flips direction; clicking another sorts ascending by it. */
export function toggleSort(current: Sort, key: Sort["key"]): Sort {
  if (current.key === key) return { ...current, desc: !current.desc };
  // Sizes and dates are more useful largest/newest first, as Explorer does.
  const desc = key === "Size" || key === "Modified" || key === "Created";
  return { ...current, key, desc };
}

export function setLayout(ws: Workspace, layout: PaneLayout): Workspace {
  if (layout === ws.layout) return ws;
  if (layout === "Single") {
    // Keep whichever pane the user was in on screen.
    const panes: [PaneState, PaneState] = ws.activePane === 1 ? [ws.panes[1], ws.panes[0]] : ws.panes;
    return { ...ws, layout, panes, activePane: 0 };
  }
  let next: Workspace = { ...ws, layout };
  if (ws.panes[1].tabs.length === 0) {
    const t = activeTab(ws, 0);
    next = withPane(next, 1, { tabs: [makeTab(t?.path ?? "C:\\", t?.view, t?.sort)], active: 0 });
  }
  return next;
}

export function focusPane(ws: Workspace, pane: 0 | 1): Workspace {
  if (pane === 1 && !isSplit(ws)) return ws;
  return ws.activePane === pane ? ws : { ...ws, activePane: pane };
}

/* ───────────────────────────── Session persistence ───────────────────────────── */

function toTabState(t: Tab): TabState {
  return { path: t.path, view: t.view, sort: t.sort, history: t.history, history_pos: t.historyPos };
}

export function toSession(ws: Workspace): SessionState {
  const visible = isSplit(ws) ? ws.panes : [ws.panes[0]];
  return {
    layout: ws.layout,
    panes: visible.map((p) => p.tabs.map(toTabState)),
    active_pane: ws.activePane,
    active_tab: visible.map((p) => p.active),
  };
}

const VIEWS: ViewMode[] = ["Details", "List", "Grid", "Tiles"];
const LAYOUTS: PaneLayout[] = ["Single", "Vertical", "Horizontal"];

/**
 * Rebuilds a workspace from a stored session, repairing anything inconsistent rather
 * than refusing to start: a session file is user state and can be from an older build.
 */
export function fromSession(s: SessionState | null | undefined, fallbackPath: string): Workspace {
  const base = initialWorkspace(fallbackPath);
  if (!s || !Array.isArray(s.panes)) return base;

  const fromTab = (t: TabState): Tab | null => {
    if (!t || typeof t.path !== "string" || !t.path) return null;
    const history = Array.isArray(t.history) && t.history.length ? t.history.map(normalize) : [normalize(t.path)];
    const pos = Math.min(Math.max(0, t.history_pos | 0), history.length - 1);
    return {
      id: newTabId(),
      path: normalize(t.path),
      view: VIEWS.includes(t.view) ? t.view : "Details",
      sort: t.sort ?? DEFAULT_SORT,
      history,
      historyPos: pos,
    };
  };

  const panes = s.panes.slice(0, 2).map((tabs, i): PaneState => {
    const list = (Array.isArray(tabs) ? tabs : []).map(fromTab).filter((t): t is Tab => t !== null);
    const active = Math.min(Math.max(0, s.active_tab?.[i] ?? 0), Math.max(0, list.length - 1));
    return { tabs: list, active };
  });

  const first = panes[0];
  if (!first || first.tabs.length === 0) return base;
  const second = panes[1] ?? { tabs: [], active: 0 };
  let layout: PaneLayout = LAYOUTS.includes(s.layout) ? s.layout : "Single";
  if (second.tabs.length === 0) layout = "Single";
  const activePane: 0 | 1 = layout !== "Single" && s.active_pane === 1 ? 1 : 0;
  return { layout, panes: [first, second], activePane, closed: [] };
}
