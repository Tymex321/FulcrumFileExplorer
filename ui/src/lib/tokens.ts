/**
 * The few design tokens TypeScript has to compute with. Kept in lockstep with
 * styles.css by `tokens.test.ts`, which parses the stylesheet.
 */
import type { Density } from "../api/types";

/** Row pixel heights per density. The virtual list's offsets depend on these exactly. */
export const ROW_HEIGHTS: Record<Density, number> = {
  Compact: 22,
  Normal: 28,
  Comfortable: 34,
};

/** Grid cell sizes for the non-details views. */
export const CELL = {
  Grid: { width: 112, height: 116 },
  Tiles: { width: 264, height: 64 },
  List: { width: 248, height: 0 /* row height */ },
} as const;
