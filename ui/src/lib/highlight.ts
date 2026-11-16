/**
 * Turns `SearchHit::match_ranges` into renderable segments.
 *
 * The matching itself already happened in Rust and is never repeated here. What is left
 * is a unit conversion that is easy to get subtly wrong: Rust reports **UTF-8 byte**
 * offsets, while JavaScript strings index **UTF-16 code units**. For an ASCII name they
 * coincide, which is exactly why a naive slice passes every test with `report_1.pdf` and
 * then bolds the wrong letters in `Résumé – 2024.docx`.
 *
 * `match_ranges` is read as flat half-open pairs `[start, end, start, end, …]`.
 */

export interface Segment {
  text: string;
  match: boolean;
}

/** Maps each UTF-8 byte offset (0..=byteLength) to a UTF-16 index into `s`. */
export function utf8ToUtf16Offsets(s: string): Uint32Array {
  // Upper bound: every UTF-16 code unit encodes to at most 3 bytes.
  const map = new Uint32Array(s.length * 3 + 1);
  let byte = 0;
  for (let i = 0; i < s.length; ) {
    const cp = s.codePointAt(i)!;
    const units = cp > 0xffff ? 2 : 1;
    const bytes = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    // Offsets that land mid-character snap back to the character's start.
    for (let b = 0; b < bytes; b++) map[byte + b] = i;
    byte += bytes;
    i += units;
  }
  map[byte] = s.length;
  return map.subarray(0, byte + 1);
}

/** Splits `text` into matched and unmatched runs. Malformed ranges are dropped, not thrown. */
export function highlightSegments(text: string, ranges: readonly number[]): Segment[] {
  if (!text) return [];
  if (ranges.length < 2) return [{ text, match: false }];

  const map = utf8ToUtf16Offsets(text);
  const maxByte = map.length - 1;
  const spans: [number, number][] = [];
  for (let i = 0; i + 1 < ranges.length; i += 2) {
    const a = Math.max(0, Math.min(ranges[i]!, maxByte));
    const b = Math.max(0, Math.min(ranges[i + 1]!, maxByte));
    if (b <= a) continue;
    spans.push([map[a]!, map[b]!]);
  }
  spans.sort((x, y) => x[0] - y[0]);

  const out: Segment[] = [];
  let pos = 0;
  for (const [a0, b] of spans) {
    const a = Math.max(a0, pos); // overlapping ranges merge rather than duplicate text
    if (b <= a) continue;
    if (a > pos) out.push({ text: text.slice(pos, a), match: false });
    const prev = out[out.length - 1];
    if (prev?.match && a === pos) prev.text += text.slice(a, b);
    else out.push({ text: text.slice(a, b), match: true });
    pos = b;
  }
  if (pos < text.length) out.push({ text: text.slice(pos), match: false });
  return out;
}

/** The inverse, for the mock backend: UTF-16 [start, end) → UTF-8 byte offsets. */
export function utf16RangeToUtf8(s: string, start: number, end: number): [number, number] {
  const enc = new TextEncoder();
  return [enc.encode(s.slice(0, start)).length, enc.encode(s.slice(0, end)).length];
}
