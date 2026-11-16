/**
 * Details-view header: click to sort (in Rust), drag an edge to resize, drag a label to
 * reorder.
 *
 * Resizing writes the `--col-*` custom property straight onto the table element during
 * the drag and commits to the store only on release. Rows size themselves from those
 * variables, so a resize drag costs one style write per frame instead of a re-render of
 * every visible row.
 */
import { type PointerEvent as RPointerEvent, useRef, useState } from "react";
import type { Sort } from "../api/types";
import { COLUMNS, type ColumnId, type ColumnLayout, dropSlot, moveColumn, resizeColumn, slotToIndex, widthOf } from "../lib/columns";
import { I } from "./Icons";

interface Props {
  layout: ColumnLayout;
  sort: Sort;
  /** Element carrying the CSS variables (the scroll container). */
  tableRef: React.RefObject<HTMLDivElement | null>;
  onSort: (col: ColumnId) => void;
  onLayout: (layout: ColumnLayout) => void;
}

export const HEADER_H = 28;

export function ColumnHeader({ layout, sort, tableRef, onSort, onLayout }: Props) {
  const [dragging, setDragging] = useState<{ from: number; slot: number; x: number } | null>(null);
  const headerRef = useRef<HTMLDivElement>(null);

  const startResize = (e: RPointerEvent, id: ColumnId) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = widthOf(layout, id);
    const el = tableRef.current;
    let latest = layout;
    const move = (ev: PointerEvent) => {
      latest = resizeColumn(layout, id, startW + ev.clientX - startX);
      el?.style.setProperty(`--col-${id}`, `${widthOf(latest, id)}px`);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.style.cursor = "";
      onLayout(latest);
    };
    document.body.style.cursor = "col-resize";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  /** Double-clicking an edge restores the default width, as in Explorer. */
  const resetWidth = (id: ColumnId) => {
    const widths = { ...layout.widths };
    delete widths[id];
    tableRef.current?.style.removeProperty(`--col-${id}`);
    onLayout({ ...layout, widths });
  };

  const startDrag = (e: RPointerEvent, index: number, id: ColumnId) => {
    if (e.button !== 0) return;
    const startX = e.clientX;
    const header = headerRef.current;
    let moved = false;
    let slot = index;
    const widths = layout.order.map((c) => widthOf(layout, c));
    const move = (ev: PointerEvent) => {
      if (!moved && Math.abs(ev.clientX - startX) < 5) return;
      moved = true;
      const rect = header?.getBoundingClientRect();
      const x = ev.clientX - (rect?.left ?? 0) + (tableRef.current?.scrollLeft ?? 0);
      slot = dropSlot(widths, x);
      let edge = 0;
      for (let i = 0; i < slot; i++) edge += widths[i]!;
      setDragging({ from: index, slot, x: edge });
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setDragging(null);
      if (!moved) onSort(id);
      else if (slot !== index && slot !== index + 1) {
        onLayout({ ...layout, order: moveColumn(layout.order, index, slotToIndex(index, slot)) });
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div
      ref={headerRef}
      role="row"
      className="sticky top-0 z-10 flex border-b border-hairline bg-base/95 text-[11px] font-medium text-ink-faint backdrop-blur"
      style={{ height: HEADER_H, minWidth: "100%" }}
    >
      {layout.order.map((id, index) => {
        const col = COLUMNS[id];
        const sorted = sort.key === col.sortKey;
        const isName = id === "name";
        return (
          <div
            key={id}
            role="columnheader"
            aria-sort={sorted ? (sort.desc ? "descending" : "ascending") : "none"}
            className={`group relative flex shrink-0 items-center transition-fast hover:bg-surface hover:text-ink-muted ${
              col.align === "right" ? "justify-end" : ""
            } ${isName ? "pl-9" : "px-3"} ${sorted ? "text-ink-muted" : ""} ${dragging?.from === index ? "opacity-50" : ""}`}
            style={{ width: `var(--col-${id})`, flex: isName ? `1 0 var(--col-${id})` : undefined }}
            onPointerDown={(e) => startDrag(e, index, id)}
          >
            <span className={`flex items-center gap-1 truncate ${col.align === "right" ? "flex-row-reverse" : ""}`}>
              {col.label}
              {sorted &&
                (sort.desc ? (
                  <I.ArrowDownSmall size={12} className="text-accent-400" />
                ) : (
                  <I.ArrowUpSmall size={12} className="text-accent-400" />
                ))}
            </span>
            <div
              className="absolute -right-1 top-0 z-10 h-full w-2 cursor-col-resize"
              onPointerDown={(e) => startResize(e, id)}
              onDoubleClick={() => resetWidth(id)}
            >
              <div className="mx-auto mt-1.5 h-4 w-px bg-hairline transition-fast group-hover:bg-hairline-strong" />
            </div>
          </div>
        );
      })}
      {dragging && (
        <div className="pointer-events-none absolute top-1 h-5 w-0.5 rounded-full bg-accent-400" style={{ left: dragging.x - 1 }} />
      )}
    </div>
  );
}
