import { describe, expect, it } from "vitest";
import { highlightSegments, utf16RangeToUtf8, utf8ToUtf16Offsets } from "./highlight";

const marked = (text: string, ranges: number[]) =>
  highlightSegments(text, ranges)
    .map((s) => (s.match ? `[${s.text}]` : s.text))
    .join("");

describe("highlightSegments", () => {
  it("marks ASCII ranges", () => {
    expect(marked("report_2024.pdf", [0, 3, 7, 11])).toBe("[rep]ort_[2024].pdf");
  });

  it("converts UTF-8 byte offsets, not code units", () => {
    // "é" is 2 bytes; "sumé" starts at byte 3 (R=1, é=2).
    const name = "Résumé.docx";
    const [a, b] = utf16RangeToUtf8(name, 2, 6);
    expect([a, b]).toEqual([3, 8]);
    expect(marked(name, [a, b])).toBe("Ré[sumé].docx");
  });

  it("handles astral characters (surrogate pairs)", () => {
    const name = "📁 notes.md"; // folder emoji is 4 bytes, 2 UTF-16 units
    const [a, b] = utf16RangeToUtf8(name, 3, 8);
    expect(marked(name, [a, b])).toBe("📁 [notes].md");
  });

  it("returns plain text when nothing matched", () => {
    expect(highlightSegments("a.txt", [])).toEqual([{ text: "a.txt", match: false }]);
  });

  it("merges overlapping and adjacent ranges rather than repeating text", () => {
    expect(marked("abcdef", [0, 3, 2, 4])).toBe("[abcd]ef");
    expect(marked("abcdef", [0, 2, 2, 4])).toBe("[abcd]ef");
  });

  it("drops malformed ranges instead of throwing", () => {
    expect(marked("abc", [2, 1])).toBe("abc");
    expect(marked("abc", [1, 999])).toBe("a[bc]");
    expect(marked("abc", [1])).toBe("abc"); // dangling start
  });

  it("snaps an offset inside a multibyte character to its start", () => {
    const map = utf8ToUtf16Offsets("é");
    expect(Array.from(map)).toEqual([0, 0, 1]);
  });
});
