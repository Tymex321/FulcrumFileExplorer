/**
 * The drag in progress, if it started inside Fulcrum.
 *
 * `DataTransfer` contents are unreadable during `dragover` (browsers only expose the
 * types), but drop targets need the source paths *while hovering* to refuse a folder
 * being dropped into itself. The payload is mirrored here for the drag's lifetime.
 */
import type { DragPayload } from "../lib/dnd";

let current: DragPayload | null = null;

export const activeDrag = () => current;
export const setActiveDrag = (p: DragPayload | null) => {
  current = p;
};

/** A tab being dragged between tab strips. */
let tabDrag: { pane: 0 | 1; index: number } | null = null;
export const activeTabDrag = () => tabDrag;
export const setActiveTabDrag = (t: typeof tabDrag) => {
  tabDrag = t;
};
