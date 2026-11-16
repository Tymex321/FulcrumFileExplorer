/**
 * Ranking for the command palette's *command* list.
 *
 * This is the only matching done in JavaScript, and deliberately so: the command list is
 * UI-owned data (a few dozen titles), not a backend result set. File search in the
 * palette always goes to Rust and renders its `match_ranges`.
 */
import type { CommandDef } from "./keymap";

export interface Ranked {
  cmd: CommandDef;
  score: number;
  /** UTF-16 [start, end) runs in the title, for highlighting. */
  runs: [number, number][];
}

/**
 * Word-prefix-aware subsequence match. "nt" finds "New tab" ahead of "Paste" because
 * both letters start words; contiguous runs score higher than scattered letters.
 */
export function matchTitle(query: string, title: string): { score: number; runs: [number, number][] } | null {
  const q = query.toLowerCase().replace(/\s+/g, "");
  if (!q) return { score: 0, runs: [] };
  const t = title.toLowerCase();
  const runs: [number, number][] = [];
  let score = 0;
  let from = 0;
  let prev = -2;
  for (const ch of q) {
    // Prefer the next word-start occurrence; fall back to any occurrence.
    let at = -1;
    for (let i = from; i < t.length; i++) {
      if (t[i] === ch && (i === 0 || /[\s:/\-.(]/.test(t[i - 1]!))) {
        at = i;
        break;
      }
    }
    if (at < 0 || (prev >= 0 && t.indexOf(ch, from) === prev + 1)) at = t.indexOf(ch, from);
    if (at < 0) return null;
    const wordStart = at === 0 || /[\s:/\-.(]/.test(t[at - 1]!);
    score += 1 + (at === prev + 1 ? 6 : 0) + (wordStart ? 8 : 0) - Math.min(3, at - from) * 0.1;
    const last = runs[runs.length - 1];
    if (last && last[1] === at) last[1] = at + 1;
    else runs.push([at, at + 1]);
    prev = at;
    from = at + 1;
  }
  // Shorter titles win ties: "Copy" over "Copy selection to other pane" for "copy".
  return { score: score - title.length * 0.02, runs };
}

export function rankCommands(query: string, commands: readonly CommandDef[], recent: readonly string[] = []): Ranked[] {
  const out: Ranked[] = [];
  for (const cmd of commands) {
    if (cmd.hidden) continue;
    const m = matchTitle(query, cmd.title) ?? matchTitle(query, `${cmd.category}: ${cmd.title}`);
    if (!m) continue;
    const r = recent.indexOf(cmd.id);
    // Recently used commands float up, most strongly with an empty query.
    const boost = r >= 0 ? (query ? 3 : 100) - r * 0.5 : 0;
    out.push({ cmd, score: m.score + boost, runs: matchTitle(query, cmd.title)?.runs ?? [] });
  }
  out.sort((a, b) => b.score - a.score || a.cmd.title.localeCompare(b.cmd.title));
  return out;
}
