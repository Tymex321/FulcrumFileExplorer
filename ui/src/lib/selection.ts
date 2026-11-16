/**
 * Multi-selection over row indices, stored as sorted disjoint inclusive ranges.
 *
 * A `Set<number>` would be the obvious choice and is wrong here: Ctrl+A on a
 * million-row directory would allocate a million-entry set and then copy it on every
 * subsequent click. Ranges make select-all, shift-click and marquee O(1) in the size of
 * the selection, and a typical selection is a handful of ranges however many rows it
 * covers.
 *
 * Immutable: every operation returns a new `Selection`, so React can compare by identity
 * and memoised rows re-render only when their own membership actually changed.
 */

export type Range = readonly [number, number];

export class Selection {
  /** Sorted, disjoint, non-adjacent inclusive ranges. */
  readonly ranges: readonly Range[];
  /** Where a shift-click range starts. */
  readonly anchor: number | null;
  /** The keyboard cursor; may sit on an unselected row after Ctrl+arrow moves. */
  readonly focus: number | null;

  private constructor(ranges: readonly Range[], anchor: number | null, focus: number | null) {
    this.ranges = ranges;
    this.anchor = anchor;
    this.focus = focus;
  }

  static readonly EMPTY = new Selection([], null, null);

  static of(ranges: readonly Range[], anchor: number | null = null, focus: number | null = anchor) {
    return new Selection(merge(ranges), anchor, focus);
  }

  get size(): number {
    let n = 0;
    for (const [a, b] of this.ranges) n += b - a + 1;
    return n;
  }

  get isEmpty(): boolean {
    return this.ranges.length === 0;
  }

  /** Binary search: called once per visible row per render. */
  has(index: number): boolean {
    let lo = 0;
    let hi = this.ranges.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const [a, b] = this.ranges[mid]!;
      if (index < a) hi = mid - 1;
      else if (index > b) lo = mid + 1;
      else return true;
    }
    return false;
  }

  /** Plain click: select exactly this row and make it the anchor. */
  select(index: number): Selection {
    return new Selection([[index, index]], index, index);
  }

  /** Ctrl-click: flip one row, and move the anchor there as Explorer does. */
  toggle(index: number): Selection {
    const ranges = this.has(index) ? subtract(this.ranges, index, index) : merge([...this.ranges, [index, index]]);
    return new Selection(ranges, index, index);
  }

  /** Shift-click: anchor-to-index replaces the selection. */
  extendTo(index: number): Selection {
    const anchor = this.anchor ?? index;
    const [a, b] = anchor <= index ? [anchor, index] : [index, anchor];
    return new Selection([[a, b]], anchor, index);
  }

  /** Ctrl+Shift-click: anchor-to-index is added to what is already selected. */
  addRange(index: number): Selection {
    const anchor = this.anchor ?? index;
    const [a, b] = anchor <= index ? [anchor, index] : [index, anchor];
    return new Selection(merge([...this.ranges, [a, b]]), anchor, index);
  }

  /** Ctrl+A. Constant-size whatever `count` is. */
  all(count: number): Selection {
    if (count <= 0) return Selection.EMPTY;
    return new Selection([[0, count - 1]], this.anchor ?? 0, this.focus ?? 0);
  }

  /** Moves the cursor without changing what is selected (Ctrl+arrow). */
  withFocus(index: number): Selection {
    return new Selection(this.ranges, this.anchor, index);
  }

  /** Union with `ranges`, for a marquee drawn with Ctrl held. */
  union(ranges: readonly Range[]): Selection {
    return new Selection(merge([...this.ranges, ...ranges]), this.anchor, this.focus);
  }

  /** Ctrl+I: everything not selected becomes selected. Still O(ranges), not O(rows). */
  invert(count: number): Selection {
    const out: Range[] = [];
    let next = 0;
    for (const [a, b] of this.ranges) {
      if (a > next) out.push([next, Math.min(a - 1, count - 1)]);
      next = b + 1;
    }
    if (next < count) out.push([next, count - 1]);
    return new Selection(out.filter(([a, b]) => a <= b), this.anchor, this.focus);
  }

  /** Drops indices at or beyond `count`, after a listing shrinks. */
  clamp(count: number): Selection {
    if (count <= 0) return Selection.EMPTY;
    const max = count - 1;
    const ranges = this.ranges
      .filter(([a]) => a <= max)
      .map(([a, b]) => [a, Math.min(b, max)] as Range);
    const fix = (i: number | null) => (i === null ? null : Math.min(i, max));
    return new Selection(ranges, fix(this.anchor), fix(this.focus));
  }

  /**
   * Selected indices in order. Callers that act on a selection (copy, delete) need the
   * concrete list, but should cap it — use `size` first for a million-row Ctrl+A.
   */
  *indices(): Generator<number> {
    for (const [a, b] of this.ranges) for (let i = a; i <= b; i++) yield i;
  }

  first(): number | null {
    return this.ranges[0]?.[0] ?? null;
  }
}

/** Sorts and merges overlapping or adjacent ranges. */
export function merge(ranges: readonly Range[]): Range[] {
  const sorted = ranges
    .map(([a, b]) => (a <= b ? [a, b] : [b, a]) as Range)
    .sort((x, y) => x[0] - y[0]);
  const out: [number, number][] = [];
  for (const [a, b] of sorted) {
    const last = out[out.length - 1];
    if (last && a <= last[1] + 1) last[1] = Math.max(last[1], b);
    else out.push([a, b]);
  }
  return out;
}

function subtract(ranges: readonly Range[], from: number, to: number): Range[] {
  const out: Range[] = [];
  for (const [a, b] of ranges) {
    if (b < from || a > to) {
      out.push([a, b]);
      continue;
    }
    if (a < from) out.push([a, from - 1]);
    if (b > to) out.push([to + 1, b]);
  }
  return out;
}

/** Rebuilds a selection from row indices, e.g. after a refresh re-finds names. */
export function fromIndices(indices: Iterable<number>, focus: number | null = null): Selection {
  const sorted = [...indices].sort((a, b) => a - b);
  const ranges: [number, number][] = [];
  for (const i of sorted) {
    const last = ranges[ranges.length - 1];
    if (last && i <= last[1] + 1) last[1] = Math.max(last[1], i);
    else ranges.push([i, i]);
  }
  const f = focus ?? sorted[0] ?? null;
  return Selection.of(ranges, f, f);
}
