/**
 * Type-to-select, as in Explorer: typing "rep" jumps to the first name starting with
 * "rep"; pressing the same letter repeatedly cycles through names starting with it.
 */

export const TYPEAHEAD_RESET_MS = 900;

export interface TypeaheadState {
  buffer: string;
  at: number;
}

export const EMPTY_TYPEAHEAD: TypeaheadState = { buffer: "", at: 0 };

/** Folds a keystroke into the buffer, starting over after a pause. */
export function feed(state: TypeaheadState, ch: string, now: number): TypeaheadState {
  const fresh = now - state.at > TYPEAHEAD_RESET_MS;
  return { buffer: (fresh ? "" : state.buffer) + ch.toLowerCase(), at: now };
}

/**
 * Finds the row to jump to. `nameAt` is an accessor rather than an array so this works
 * directly against the listing store without materialising names. A linear scan over a
 * million names takes a few milliseconds, well under a keystroke.
 */
export function findMatch(
  buffer: string,
  count: number,
  nameAt: (i: number) => string,
  current: number | null,
): number | null {
  if (!buffer || count <= 0) return null;
  // "aaa" means "cycle through items starting with a", not "find aaa".
  const repeated = buffer.length > 1 && [...buffer].every((c) => c === buffer[0]);
  const needle = repeated ? buffer[0]! : buffer;
  // A fresh single letter or a repeat moves past the current row; a growing prefix
  // re-tests the current row so "re" → "rep" stays put if it still matches.
  const startOffset = needle.length === 1 ? 1 : 0;
  const start = current === null ? 0 : current + startOffset;
  for (let k = 0; k < count; k++) {
    const i = (start + k) % count;
    if (nameAt(i).toLowerCase().startsWith(needle)) return i;
  }
  return null;
}
