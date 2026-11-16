/**
 * The file table: details, list, tiles and grid views over one virtualised surface.
 *
 * Rules that keep it smooth at a million rows:
 *
 * 1. **Only the visible window is in the DOM**, and row positions come from maths
 *    (`lib/virtual.ts`), including past the browser's element-height limit.
 * 2. **Rows are read by index from the tab runtime**, never copied into React state.
 * 3. **One set of event handlers on the container**, not per row. Rows carry
 *    `data-index`; handlers find it with `closest`. Per-row closures would defeat
 *    `memo` and allocate thousands of functions per second while scrolling.
 * 4. **Row visuals are driven by data attributes**, so a selection change re-renders
 *    only the rows whose membership flipped.
 */
import {
  type DragEvent as RDragEvent,
  memo,
  type MouseEvent as RMouseEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { FileEntry, SearchHit } from "../api/types";
import { COLUMNS, type ColumnId, columnVars, totalWidth } from "../lib/columns";
import { DRAG_MIME, decodePayload, dropOperation, dropRejection, encodePayload } from "../lib/dnd";
import { typeInfo } from "../lib/fileTypes";
import { formatDate, formatSize } from "../lib/format";
import { highlightSegments } from "../lib/highlight";
import { parent, stemLength } from "../lib/path";
import { Selection } from "../lib/selection";
import { CELL, ROW_HEIGHTS } from "../lib/tokens";
import {
  cellsInRect,
  edgeScrollSpeed,
  type GridWindow,
  gridWindow,
  type Move,
  moveFocus,
  rectFrom,
  rowsInRect,
  scrollToIndex,
  toPhysical,
} from "../lib/virtual";
import { type CommandContext, TYPEAHEAD } from "../lib/keymap";
import {
  commitRename,
  dropPaths,
  focusSearch,
  isCut,
  openItem,
  openMenu,
  type PaneId,
  updateWorkspace,
  useApp,
} from "../state/app";
import { useCommand, useCommands } from "../state/commands";
import { setLayout, useColumns } from "../state/columns";
import { activeDrag, setActiveDrag } from "../state/drag";
import type { TabRuntime } from "../state/tabRuntime";
import { useThumbnail } from "../state/thumbs";
import * as W from "../lib/workspace";
import { ColumnHeader, HEADER_H } from "./ColumnHeader";
import { FileIcon, I } from "./Icons";
import { useViewport } from "./useViewport";

interface Props {
  pane: PaneId;
  tab: W.Tab;
  rt: TabRuntime;
  active: boolean;
}

const MOVES: Move[] = ["up", "down", "left", "right", "pageUp", "pageDown", "home", "end"];

interface Marquee {
  /** Anchor in content coordinates (virtual pixels for y). */
  ax: number;
  ay: number;
  bx: number;
  by: number;
  base: Selection;
  additive: boolean;
}

export function FileView({ pane, tab, rt, active }: Props) {
  const snap = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  const density = useApp((s) => s.settings?.appearance.density ?? "Normal");
  const singleClick = useApp((s) => s.settings?.behavior.single_click_open ?? false);
  const clipboard = useApp((s) => s.clipboard);
  const rowH = ROW_HEIGHTS[density];

  const scrollRef = useRef<HTMLDivElement>(null);
  const vp = useViewport(scrollRef);

  const isSearch = snap.mode === "search";
  const view = tab.view;
  const details = view === "Details";
  const colKind = isSearch ? "search" : "list";
  const layout = useColumns((c) => c[colKind]);

  // Geometry per view. Details is a one-column grid; the maths is shared.
  const top = details ? HEADER_H : 8;
  const left = details ? 0 : 8;
  const cellW = details ? Math.max(1, vp.width) : view === "List" ? CELL.List.width : CELL[view].width;
  const cellH = details || view === "List" ? rowH : CELL[view].height;
  const innerW = Math.max(1, vp.width - left * 2);
  const viewH = Math.max(0, vp.height - top);
  const win: GridWindow = gridWindow(vp.scrollTop, viewH, innerW, cellW, cellH, snap.count, details ? 8 : 2);
  const pageRows = Math.max(1, Math.floor(viewH / cellH) - 1);

  // Keep a ref to the latest geometry for handlers that run outside render.
  const geo = useRef({ win, cellW, cellH, top, left, viewH, pageRows });
  geo.current = { win, cellW, cellH, top, left, viewH, pageRows };

  /* ───────────── Scroll requests (reveal a row after keyboard moves or loads) ───────────── */

  useEffect(() => {
    const r = snap.reveal;
    const el = scrollRef.current;
    if (!r || !el) return;
    const g = geo.current;
    const row = Math.floor(r.index / g.win.columns);
    const target = scrollToIndex(row, g.win.virtualTop, g.viewH, g.cellH);
    if (r.index === 0 && row === 0) el.scrollTop = 0;
    else if (target !== null) el.scrollTop = toPhysical(g.win.model, target);
  }, [snap.reveal]);

  // Infinite scroll for paged search results.
  useEffect(() => {
    if (isSearch && win.end >= snap.count - 200) rt.loadMore();
  }, [isSearch, win.end, snap.count, rt]);

  /* ───────────────────────────── Selection helpers ───────────────────────────── */

  const select = useCallback((sel: Selection, reveal = false) => rt.setSelection(sel, reveal), [rt]);

  const clickSelect = (index: number, e: { ctrlKey: boolean; shiftKey: boolean; metaKey: boolean }) => {
    const s = rt.getSnapshot().selection;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.shiftKey) select(s.addRange(index));
    else if (e.shiftKey) select(s.extendTo(index));
    else if (ctrl) select(s.toggle(index));
    else select(s.select(index));
  };

  const openIndex = (index: number, newTab = false) => {
    const entry = rt.entryAt(index);
    const path = rt.pathAt(index);
    if (!entry || !path) return;
    void openItem(path, entry.kind === "Dir" || entry.kind === "Volume", pane, newTab);
  };

  /* ───────────────────────────── Keyboard ───────────────────────────── */

  const keyboardEnabled = active && snap.renaming === null;
  const moveHandlers: Record<string, (ctx: CommandContext) => boolean | void> = {};
  for (const m of MOVES) {
    const target = () => {
      const s = rt.getSnapshot();
      const g = geo.current;
      return moveFocus(s.selection.focus, m, s.count, g.win.columns, g.pageRows);
    };
    moveHandlers[`list.${m}`] = () => {
      const t = target();
      if (t === null) return false;
      select(rt.getSnapshot().selection.select(t), true);
    };
    moveHandlers[`list.${m}.extend`] = () => {
      const t = target();
      if (t === null) return false;
      select(rt.getSnapshot().selection.extendTo(t), true);
    };
    moveHandlers[`list.${m}.cursor`] = () => {
      const t = target();
      if (t === null) return false;
      select(rt.getSnapshot().selection.withFocus(t), true);
    };
  }

  // File-list handlers never act on keystrokes typed into a field (search box, path
  // editor): those keys belong to the caret.
  const guard = (f: (ctx: CommandContext) => boolean | void) => (ctx: CommandContext) => (ctx.inInput ? false : f(ctx));
  useCommands(
    Object.fromEntries(
      Object.entries({
      ...moveHandlers,
      "list.open": () => {
        const f = rt.getSnapshot().selection.focus;
        if (f === null) return false;
        openIndex(f);
      },
      "list.openNewTab": () => {
        const f = rt.getSnapshot().selection.focus;
        if (f === null) return false;
        openIndex(f, true);
      },
      "list.selectAll": () => select(rt.getSnapshot().selection.all(rt.count)),
      "list.invert": () => select(rt.getSnapshot().selection.invert(rt.count)),
      "list.toggle": () => {
        const f = rt.getSnapshot().selection.focus;
        if (f === null) return false;
        select(rt.getSnapshot().selection.toggle(f));
      },
      "file.rename": () => rt.startRename(),
      [TYPEAHEAD]: (ctx: CommandContext) => {
        if (!ctx.text) return false;
        // In a search, typing refines the query instead of jumping.
        if (rt.getSnapshot().mode === "search") {
          focusSearch(pane);
          return false;
        }
        rt.typeAhead(ctx.text);
      },
      "ui.cancel": () => {
        const s = rt.getSnapshot();
        if (s.mode === "search") {
          rt.clearSearch();
          return;
        }
        if (s.selection.isEmpty) return false;
        select(Selection.EMPTY);
      },
      }).map(([id, f]) => [id, guard(f)]),
    ),
    keyboardEnabled,
  );

  /* ───────────────────────────── Mouse: click, marquee ───────────────────────────── */

  const [marquee, setMarquee] = useState<Marquee | null>(null);

  const indexFrom = (target: EventTarget | null): number | null => {
    const el = (target as HTMLElement | null)?.closest?.("[data-index]");
    if (!el) return null;
    const i = Number((el as HTMLElement).dataset.index);
    return Number.isFinite(i) ? i : null;
  };

  /** Pointer position in content coordinates: x from the grid's left, y in virtual px. */
  const contentPoint = (clientX: number, clientY: number) => {
    const el = scrollRef.current!;
    const rect = el.getBoundingClientRect();
    const g = geo.current;
    return {
      x: clientX - rect.left - g.left + el.scrollLeft,
      y: clientY - rect.top - g.top + g.win.virtualTop,
    };
  };

  const marqueeSelection = (m: Marquee): Selection => {
    const g = geo.current;
    const rect = rectFrom(m.ax, m.ay, m.bx, m.by);
    const count = rt.count;
    let ranges: [number, number][];
    if (g.win.columns === 1) {
      const r = rowsInRect(rect, g.cellH, count);
      ranges = r ? [r] : [];
    } else {
      ranges = cellsInRect(rect, g.win.columns, g.cellW, g.cellH, count);
    }
    const focus = ranges.length ? ranges[ranges.length - 1]![1] : m.base.focus;
    const base = m.additive ? m.base : Selection.EMPTY;
    return base.union(ranges).withFocus(focus ?? 0);
  };

  const onMouseDown = (e: RMouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest("input, [role=columnheader]")) return;
    scrollRef.current?.focus({ preventScroll: true });
    if (!active) updateWorkspace((ws) => W.focusPane(ws, pane));

    const index = indexFrom(target);
    const onHandle = !!target.closest("[data-handle]");
    const ctrl = e.ctrlKey || e.metaKey;

    if (index !== null && onHandle) {
      if (singleClick && !ctrl && !e.shiftKey) {
        select(rt.getSnapshot().selection.select(index));
        return;
      }
      // Clicking an already-selected row without modifiers narrows the selection on
      // release, not on press, so the whole selection can still be dragged.
      if (!ctrl && !e.shiftKey && rt.getSnapshot().selection.has(index)) {
        const up = (ev: MouseEvent) => {
          window.removeEventListener("mouseup", up);
          if (Math.abs(ev.clientX - e.clientX) < 4 && Math.abs(ev.clientY - e.clientY) < 4) {
            select(rt.getSnapshot().selection.select(index));
          }
        };
        window.addEventListener("mouseup", up);
        return;
      }
      clickSelect(index, e);
      return;
    }

    // Anywhere else — empty space, or a row outside its name — begins a marquee.
    e.preventDefault();
    const start = contentPoint(e.clientX, e.clientY);
    const base = rt.getSnapshot().selection;
    let current: Marquee | null = null;
    let lastY = e.clientY;
    let lastX = e.clientX;
    let raf = 0;

    const update = () => {
      if (!current) return;
      const p = contentPoint(lastX, lastY);
      current = { ...current, bx: p.x, by: p.y };
      setMarquee(current);
      select(marqueeSelection(current));
    };
    const autoScroll = () => {
      const el = scrollRef.current;
      if (!el || !current) return;
      const rect = el.getBoundingClientRect();
      const v = edgeScrollSpeed(lastY, rect.top + geo.current.top, rect.bottom);
      if (v !== 0) {
        el.scrollTop += v;
        update();
      }
      raf = requestAnimationFrame(autoScroll);
    };
    const move = (ev: MouseEvent) => {
      lastX = ev.clientX;
      lastY = ev.clientY;
      if (!current) {
        if (Math.abs(ev.clientX - e.clientX) < 4 && Math.abs(ev.clientY - e.clientY) < 4) return;
        current = { ax: start.x, ay: start.y, bx: start.x, by: start.y, base, additive: ctrl };
        raf = requestAnimationFrame(autoScroll);
      }
      update();
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      cancelAnimationFrame(raf);
      if (current) {
        setMarquee(null);
        return;
      }
      // A plain click: on a row selects it, on empty space clears.
      if (index !== null) clickSelect(index, e);
      else if (!ctrl && !e.shiftKey) select(Selection.EMPTY);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const onDoubleClick = (e: RMouseEvent) => {
    const index = indexFrom(e.target);
    if (index !== null && !(e.target as HTMLElement).closest("input")) openIndex(index);
  };

  const onClick = (e: RMouseEvent) => {
    if (!singleClick) return;
    const index = indexFrom(e.target);
    if (index !== null && (e.target as HTMLElement).closest("[data-handle]") && !e.ctrlKey && !e.shiftKey) openIndex(index);
  };

  const onAuxClick = (e: RMouseEvent) => {
    // Middle-click a folder opens it in a background tab.
    if (e.button !== 1) return;
    const index = indexFrom(e.target);
    const entry = index !== null ? rt.entryAt(index) : undefined;
    const path = index !== null ? rt.pathAt(index) : undefined;
    if (entry?.kind === "Dir" && path) {
      e.preventDefault();
      updateWorkspace((ws) => W.openTab(ws, pane, path, { activate: false }));
    }
  };

  const onContextMenu = (e: RMouseEvent) => {
    e.preventDefault();
    if (!active) updateWorkspace((ws) => W.focusPane(ws, pane));
    scrollRef.current?.focus({ preventScroll: true });
    const index = indexFrom(e.target);
    const s = rt.getSnapshot().selection;
    if (index !== null && !s.has(index)) select(s.select(index));
    openMenu({ x: e.clientX, y: e.clientY, pane, onRow: index !== null });
  };

  /* ───────────────────────────── Drag and drop ───────────────────────────── */

  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const [dropOnPane, setDropOnPane] = useState(false);

  const onDragStart = (e: RDragEvent) => {
    const index = indexFrom(e.target);
    if (index === null) return;
    let s = rt.getSnapshot().selection;
    if (!s.has(index)) {
      s = s.select(index);
      select(s);
    }
    const paths = rt.selectedPaths(50_000);
    const payload = { paths, sourceDir: tab.path };
    setActiveDrag(payload);
    e.dataTransfer.setData(DRAG_MIME, encodePayload(payload));
    e.dataTransfer.setData("text/plain", paths.join("\n"));
    e.dataTransfer.effectAllowed = "copyMove";
    // A compact "3 items" chip reads better than a ghost of the row.
    const ghost = document.createElement("div");
    ghost.textContent = paths.length === 1 ? (rt.entryAt(index)?.name ?? "1 item") : `${paths.length.toLocaleString()} items`;
    Object.assign(ghost.style, {
      position: "fixed",
      top: "-100px",
      padding: "4px 10px",
      borderRadius: "6px",
      font: "500 12px var(--font-ui)",
      color: "white",
      background: "var(--color-accent-600)",
    });
    document.body.appendChild(ghost);
    e.dataTransfer.setDragImage(ghost, -10, -10);
    setTimeout(() => ghost.remove(), 0);
  };

  const onDragEnd = () => {
    setActiveDrag(null);
    setDropIndex(null);
    setDropOnPane(false);
  };

  /** Resolves where a drop at this point would go: a folder row, or this pane's folder. */
  const dropTarget = (e: RDragEvent): { dir: string; index: number | null } | null => {
    const index = indexFrom(e.target);
    if (index !== null) {
      const entry = rt.entryAt(index);
      const path = rt.pathAt(index);
      if (entry?.kind === "Dir" && path) return { dir: path, index };
    }
    if (rt.getSnapshot().mode === "search") return null; // no single folder to drop into
    return { dir: tab.path, index: null };
  };

  const onDragOver = (e: RDragEvent) => {
    if (!e.dataTransfer.types.includes(DRAG_MIME)) return;
    const t = dropTarget(e);
    const drag = activeDrag();
    const mods = { ctrl: e.ctrlKey, shift: e.shiftKey };
    const op = t && drag ? dropOperation(drag.paths, t.dir, mods) : null;
    const rejected = !t || !drag || !op || dropRejection(drag.paths, t.dir, op) !== null;
    if (rejected) {
      e.dataTransfer.dropEffect = "none";
      if (dropIndex !== null) setDropIndex(null);
      if (dropOnPane) setDropOnPane(false);
      return;
    }
    e.preventDefault();
    e.dataTransfer.dropEffect = op === "Copy" ? "copy" : "move";
    if (dropIndex !== t.index) setDropIndex(t.index);
    const onPane = t.index === null;
    if (dropOnPane !== onPane) setDropOnPane(onPane);
  };

  const onDragLeave = (e: RDragEvent) => {
    if (!scrollRef.current?.contains(e.relatedTarget as Node | null)) {
      setDropIndex(null);
      setDropOnPane(false);
    }
  };

  const onDrop = (e: RDragEvent) => {
    const payload = decodePayload(e.dataTransfer.getData(DRAG_MIME));
    const t = dropTarget(e);
    onDragEnd();
    if (!payload || !t) return;
    e.preventDefault();
    dropPaths(payload.paths, t.dir, { ctrl: e.ctrlKey, shift: e.shiftKey });
  };

  /* ───────────────────────────── Render ───────────────────────────── */

  const sel = snap.selection;
  const items: React.ReactNode[] = [];
  for (let i = win.start; i < win.end; i++) {
    const entry = rt.entryAt(i);
    if (!entry) continue;
    const path = rt.pathAt(i) ?? "";
    const common = {
      index: i,
      entry,
      hit: rt.hitAt(i),
      selected: sel.has(i),
      focused: sel.focus === i,
      cut: clipboard?.mode === "cut" && isCut(path, clipboard),
      drop: dropIndex === i,
      renaming: snap.renaming === i,
      rt,
    };
    if (details) {
      items.push(<DetailsRow key={i} {...common} order={layout.order} />);
    } else {
      const local = i - win.start;
      const row = Math.floor(local / win.columns);
      const col = local % win.columns;
      items.push(
        <GridCell
          key={i}
          {...common}
          view={view}
          path={path}
          style={{ left: col * cellW, top: row * cellH, width: cellW - (view === "List" ? 0 : 6), height: cellH - (view === "List" ? 0 : 6) }}
        />,
      );
    }
  }

  const contentWidth = details ? totalWidth(layout) : undefined;
  const vars = details ? columnVars(layout) : {};

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        tabIndex={0}
        role={details ? "grid" : "listbox"}
        aria-multiselectable
        aria-rowcount={snap.count}
        aria-label={isSearch ? "Search results" : `Contents of ${tab.path}`}
        className={`list relative min-h-0 flex-1 overflow-auto ${dropOnPane ? "bg-[color-mix(in_oklab,var(--color-drop)_35%,transparent)]" : ""}`}
        style={vars as React.CSSProperties}
        onMouseDown={onMouseDown}
        onDoubleClick={onDoubleClick}
        onClick={onClick}
        onAuxClick={onAuxClick}
        onContextMenu={onContextMenu}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
      >
        {details && (
          <ColumnHeader
            layout={layout}
            sort={tab.sort}
            tableRef={scrollRef}
            onSort={(id) => updateWorkspace((ws) => W.setSort(W.focusPane(ws, pane), pane, W.toggleSort(tab.sort, COLUMNS[id].sortKey)))}
            onLayout={(l) => setLayout(colKind, l)}
          />
        )}
        {/* Spacer: carries the (possibly scaled) full height for an honest scrollbar. */}
        <div style={{ height: win.totalHeight + (details ? 0 : 16), minWidth: contentWidth, position: "relative" }}>
          <div
            className="absolute"
            style={{
              top: details ? 0 : top,
              left,
              right: details ? undefined : left,
              minWidth: details ? "100%" : undefined,
              transform: `translateY(${win.offsetY}px)`,
            }}
          >
            {items}
          </div>
        </div>
        {marquee && <MarqueeBox m={marquee} virtualTop={win.virtualTop} scrollTop={vp.scrollTop} top={top} left={left} />}
      </div>
      <EmptyState rt={rt} />
    </div>
  );
}

/* ───────────────────────────── Rows ───────────────────────────── */

interface RowProps {
  index: number;
  entry: FileEntry;
  hit: SearchHit | undefined;
  selected: boolean;
  focused: boolean;
  cut: boolean;
  drop: boolean;
  renaming: boolean;
  rt: TabRuntime;
}

const DetailsRow = memo(function DetailsRow(props: RowProps & { order: ColumnId[] }) {
  const { index, entry, hit, selected, focused, cut, drop, renaming, rt, order } = props;
  const info = typeInfo(entry);
  return (
    <div
      role="row"
      aria-selected={selected}
      data-index={index}
      data-selected={selected}
      data-focused={focused}
      data-cut={cut}
      data-drop={drop}
      className="row"
    >
      {order.map((id) => {
        switch (id) {
          case "name":
            return (
              <div key={id} className="flex min-w-0 items-center pl-3 pr-2" style={{ flex: "1 0 var(--col-name)", width: "var(--col-name)" }}>
                {renaming ? (
                  <span className="flex min-w-0 flex-1 items-center gap-2">
                    <FileIcon kind={info.icon} />
                    <RenameField rt={rt} index={index} entry={entry} />
                  </span>
                ) : (
                  <span data-handle draggable className="flex min-w-0 items-center gap-2 rounded py-0.5 pr-1">
                    <FileIcon kind={info.icon} />
                    <Name entry={entry} hit={hit} />
                  </span>
                )}
              </div>
            );
          case "size":
            return (
              <div key={id} className="meta shrink-0 truncate px-3 text-right" style={{ width: "var(--col-size)" }}>
                {entry.kind === "Dir" ? "" : formatSize(entry.size)}
              </div>
            );
          case "type":
            return (
              <div key={id} className="meta shrink-0 truncate px-3" style={{ width: "var(--col-type)" }}>
                {info.label}
              </div>
            );
          case "modified":
            return (
              <div key={id} className="meta shrink-0 truncate px-3 text-right" style={{ width: "var(--col-modified)" }}>
                {formatDate(entry.modified)}
              </div>
            );
          case "created":
            return (
              <div key={id} className="meta shrink-0 truncate px-3 text-right" style={{ width: "var(--col-created)" }}>
                {formatDate(entry.created)}
              </div>
            );
          case "path":
            return (
              <div key={id} className="meta shrink-0 truncate px-3" style={{ width: "var(--col-path)", direction: "rtl", textAlign: "left" }} title={hit ? (parent(hit.path) ?? "") : ""}>
                {/* rtl keeps the *end* of a long folder path visible — the part that differs. */}
                <bdi>{hit ? (parent(hit.path) ?? "") : ""}</bdi>
              </div>
            );
        }
      })}
    </div>
  );
});

/** Name with search-match highlighting from `match_ranges` — never re-matched in JS. */
function Name({ entry, hit, wrap }: { entry: FileEntry; hit: SearchHit | undefined; wrap?: boolean }) {
  // Grid labels wrap onto two lines; everywhere else names truncate with an ellipsis.
  const dim = `${entry.attrs.hidden ? "opacity-60" : ""} ${wrap ? "" : "truncate"}`;
  if (!hit || hit.match_ranges.length === 0) return <span className={dim}>{entry.name}</span>;
  return (
    <span className={dim}>
      {highlightSegments(entry.name, hit.match_ranges).map((s, i) =>
        s.match ? (
          <mark key={i} className="match">
            {s.text}
          </mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </span>
  );
}

const GridCell = memo(function GridCell(props: RowProps & { view: "List" | "Grid" | "Tiles"; path: string; style: React.CSSProperties }) {
  const { index, entry, hit, selected, focused, cut, drop, renaming, rt, view, path, style } = props;
  const info = typeInfo(entry);
  const thumb = useThumbnail(view === "Grid" && info.icon === "image" ? path : undefined, 96, view === "Grid");

  if (view === "List") {
    return (
      <div role="option" aria-selected={selected} data-index={index} data-selected={selected} data-focused={focused} data-cut={cut} data-drop={drop} className="row absolute rounded px-2" style={style}>
        {renaming ? (
          <span className="flex min-w-0 flex-1 items-center gap-2">
            <FileIcon kind={info.icon} />
            <RenameField rt={rt} index={index} entry={entry} />
          </span>
        ) : (
          <span data-handle draggable className="flex min-w-0 items-center gap-2">
            <FileIcon kind={info.icon} />
            <Name entry={entry} hit={hit} />
          </span>
        )}
      </div>
    );
  }

  if (view === "Tiles") {
    return (
      <div role="option" aria-selected={selected} data-index={index} data-selected={selected} data-focused={focused} data-cut={cut} data-drop={drop} className="cell absolute flex items-center gap-3 px-3" style={style}>
        <span data-handle draggable className="grid size-10 shrink-0 place-items-center rounded-md bg-surface">
          <FileIcon kind={info.icon} size={26} />
        </span>
        <div className="min-w-0 flex-1 text-xs">
          {renaming ? <RenameField rt={rt} index={index} entry={entry} /> : <div data-handle draggable className="truncate text-ink"><Name entry={entry} hit={hit} /></div>}
          <div className="meta truncate text-[11px]">{info.label}</div>
          <div className="meta truncate text-[11px]">{entry.kind === "Dir" ? formatDate(entry.modified) : formatSize(entry.size)}</div>
        </div>
      </div>
    );
  }

  return (
    <div role="option" aria-selected={selected} data-index={index} data-selected={selected} data-focused={focused} data-cut={cut} data-drop={drop} className="cell absolute flex flex-col items-center px-1.5 pt-2" style={style}>
      <span data-handle draggable className="grid h-16 w-full place-items-center overflow-hidden rounded-md">
        {thumb ? (
          <img src={thumb} alt="" draggable={false} className="max-h-16 max-w-full rounded-[3px] object-contain shadow-[0_1px_4px_rgb(0_0_0/0.5)]" />
        ) : (
          <FileIcon kind={info.icon} size={44} />
        )}
      </span>
      <div className="mt-1.5 w-full text-center text-[11px] leading-tight">
        {renaming ? (
          <RenameField rt={rt} index={index} entry={entry} />
        ) : (
          <span data-handle draggable className="line-clamp-2 block [overflow-wrap:anywhere]">
            <Name entry={entry} hit={hit} wrap />
          </span>
        )}
      </div>
    </div>
  );
});

/** Inline rename: Enter commits (form submit), Escape cancels (registry), blur commits. */
function RenameField({ rt, index, entry }: { rt: TabRuntime; index: number; entry: FileEntry }) {
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    // Select the stem, not the extension: renaming "report.pdf" should not retype ".pdf".
    el.setSelectionRange(0, stemLength(entry.name, entry.kind === "Dir"));
  }, [entry]);

  const finish = (commit: boolean) => {
    if (done.current) return;
    done.current = true;
    const value = ref.current?.value ?? entry.name;
    (document.querySelector(`[data-pane-active="true"] .list`) as HTMLElement | null)?.focus({ preventScroll: true });
    if (commit) void commitRename(rt, index, value.trim());
    else rt.endRename();
  };

  useCommand("ui.cancel", () => finish(false));

  return (
    <form
      className="min-w-0 flex-1"
      onSubmit={(e) => {
        e.preventDefault();
        finish(true);
      }}
    >
      <input
        ref={ref}
        defaultValue={entry.name}
        spellCheck={false}
        aria-label="New name"
        onBlur={() => finish(true)}
        onMouseDown={(e) => e.stopPropagation()}
        className="h-[calc(var(--row-h)-6px)] w-full rounded-[4px] bg-base px-1.5 text-xs text-ink shadow-[inset_0_0_0_1px_var(--color-accent-500),0_0_0_3px_rgb(139_92_246/0.25)] outline-none"
      />
    </form>
  );
}

function MarqueeBox({ m, virtualTop, scrollTop, top, left }: { m: Marquee; virtualTop: number; scrollTop: number; top: number; left: number }) {
  const r = rectFrom(m.ax, m.ay, m.bx, m.by);
  // Content (virtual) y → the scroll element's physical content y. x already includes
  // the horizontal scroll offset.
  const y0 = r.y0 - virtualTop + top + scrollTop;
  const y1 = r.y1 - virtualTop + top + scrollTop;
  return (
    <div
      className="pointer-events-none absolute z-20 rounded-[2px] border"
      style={{
        left: r.x0 + left,
        top: y0,
        width: r.x1 - r.x0,
        height: Math.max(0, y1 - y0),
        borderColor: "var(--color-marquee-edge)",
        background: "var(--color-marquee)",
      }}
    />
  );
}

/* ───────────────────────────── Empty, loading and error states ───────────────────────────── */

function EmptyState({ rt }: { rt: TabRuntime }) {
  const s = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  if (s.count > 0) return null;

  let icon = <I.Doc size={22} />;
  let title = "";
  let body = "";
  if (s.error) {
    icon = <I.Warn size={22} />;
    switch (s.error.kind) {
      case "AccessDenied":
        title = "You don't have permission to open this folder";
        body = "Windows denied access. Fulcrum never elevates the main window.";
        break;
      case "NotFound":
        title = "This location doesn't exist";
        body = "It may have been moved, renamed or the drive disconnected.";
        break;
      default:
        title = "Couldn't read this folder";
        body = "detail" in s.error ? s.error.detail : s.error.kind;
    }
  } else if (s.loading) {
    return <LoadingRows />;
  } else if (s.mode === "search") {
    icon = <I.Search size={22} />;
    title = `No results for “${s.search.text}”`;
    body = s.search.scope === "here" ? "Try searching everywhere, or loosen the filters." : "Try fewer words or a fuzzy match.";
  } else {
    icon = <I.FolderPlus size={22} />;
    title = "This folder is empty";
    body = "Drop files here, or press Ctrl+Shift+N for a new folder.";
  }

  return (
    <div className="pointer-events-none absolute inset-0 top-8 grid place-items-center" style={{ animation: "fade-in var(--dur) var(--ease-out)" }}>
      <div className="flex max-w-xs flex-col items-center gap-2 text-center">
        <div className="grid size-11 place-items-center rounded-xl bg-surface text-ink-faint shadow-[inset_0_0_0_1px_var(--color-hairline)]">{icon}</div>
        <div className="text-[13px] font-medium text-ink">{title}</div>
        <div className="text-xs text-ink-muted">{body}</div>
      </div>
    </div>
  );
}

/** Skeleton rows while a slow folder reads: the shape of the answer, without a spinner. */
function LoadingRows() {
  return (
    <div className="pointer-events-none absolute inset-x-0 top-8 px-3" aria-label="Loading">
      {Array.from({ length: 8 }, (_, i) => (
        <div key={i} className="flex items-center gap-2" style={{ height: "var(--row-h)", opacity: 1 - i * 0.11 }}>
          <div className="size-4 rounded bg-raised" />
          <div className="h-2.5 rounded-full bg-raised" style={{ width: `${30 + ((i * 37) % 40)}%` }} />
        </div>
      ))}
    </div>
  );
}

