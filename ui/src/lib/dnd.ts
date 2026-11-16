/**
 * Drag-and-drop semantics, matching Explorer so muscle memory carries over:
 * a plain drag **moves** within a volume and **copies** across volumes; Ctrl forces a
 * copy, Shift forces a move.
 */
import { isWithin, normalize, parent, volumeOf } from "./path";

/** MIME type carrying dragged paths between panes. Custom, so foreign drops are ignored. */
export const DRAG_MIME = "application/x-fulcrum-paths";

export interface DragPayload {
  paths: string[];
  /** Directory the drag started in, to reject no-op drops onto the same folder. */
  sourceDir: string;
}

export type DropOp = "Copy" | "Move";

export function dropOperation(
  sources: readonly string[],
  destDir: string,
  mods: { ctrl: boolean; shift: boolean },
): DropOp {
  if (mods.ctrl) return "Copy";
  if (mods.shift) return "Move";
  const destVol = volumeOf(destDir);
  const sameVolume = sources.every((s) => volumeOf(s) === destVol);
  return sameVolume ? "Move" : "Copy";
}

/**
 * Why a drop must be refused, or `null` when it is allowed. Dropping a folder into
 * itself or its own descendant would recurse forever; moving into the folder the items
 * are already in is a no-op that would still raise a conflict for every file.
 */
export function dropRejection(sources: readonly string[], destDir: string, op: DropOp): string | null {
  if (sources.length === 0) return "Nothing to drop";
  for (const s of sources) {
    if (isWithin(destDir, s)) return "Cannot drop a folder into itself";
  }
  if (op === "Move" && sources.every((s) => normalize(parent(s) ?? "") === normalize(destDir))) {
    return "Already in this folder";
  }
  return null;
}

export function encodePayload(p: DragPayload): string {
  return JSON.stringify(p);
}

export function decodePayload(raw: string): DragPayload | null {
  try {
    const v: unknown = JSON.parse(raw);
    if (
      v &&
      typeof v === "object" &&
      Array.isArray((v as DragPayload).paths) &&
      (v as DragPayload).paths.every((p) => typeof p === "string") &&
      typeof (v as DragPayload).sourceDir === "string"
    ) {
      return v as DragPayload;
    }
  } catch {
    /* not ours */
  }
  return null;
}
