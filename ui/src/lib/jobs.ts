/**
 * How a file operation's progress is described. Pure, because the honesty rules are the
 * point and deserve tests: no percentage while the operation is still counting, and
 * "estimating…" — never a made-up number — while `eta_secs` is `null`.
 */
import type { FsOpRequest, Progress } from "../api/types";
import { formatEta, formatRate, formatSize } from "./format";
import { basename } from "./path";

const VERB: Record<FsOpRequest["kind"], [string, string]> = {
  Copy: ["Copying", "Copied"],
  Move: ["Moving", "Moved"],
  Delete: ["Deleting", "Deleted"],
  Trash: ["Moving to Recycle Bin", "Moved to Recycle Bin"],
};

export function jobTitle(req: FsOpRequest, p: Progress | null): string {
  const done = p?.state === "Done";
  const verb = VERB[req.kind][done ? 1 : 0];
  const what = req.sources.length === 1 ? `“${basename(req.sources[0]!)}”` : `${req.sources.length.toLocaleString()} items`;
  const to = req.destination && (req.kind === "Copy" || req.kind === "Move") ? ` to ${basename(req.destination)}` : "";
  if (p?.state === "Cancelled") return `Cancelled: ${VERB[req.kind][0].toLowerCase()} ${what}`;
  if (p?.state === "Failed") return `Failed: ${VERB[req.kind][0].toLowerCase()} ${what}`;
  return `${verb} ${what}${to}`;
}

/**
 * Completed fraction in 0..1, or `null` when no honest fraction exists yet (still
 * scanning, or nothing measurable). Byte-weighted when there are bytes, since one 40 GB
 * file and a thousand tiny ones should not count the same.
 */
export function fraction(p: Progress | null): number | null {
  if (!p || p.state === "Pending" || p.state === "Scanning") return null;
  if (p.state === "Done") return 1;
  if (p.total_bytes > 0) return Math.min(1, p.done_bytes / p.total_bytes);
  if (p.total_items > 0) return Math.min(1, p.done_items / p.total_items);
  return null;
}

export function detailLine(p: Progress | null): string {
  if (!p || p.state === "Pending") return "Starting…";
  if (p.state === "Scanning") {
    const found = p.total_items ? `${p.total_items.toLocaleString()} items` : "items";
    return `Counting ${found}${p.total_bytes ? ` · ${formatSize(p.total_bytes)}` : ""}…`;
  }
  if (p.state === "Paused") return p.pending_conflict ? "Waiting for your decision" : "Paused";
  if (p.state === "Done") {
    return p.total_bytes ? `${formatSize(p.total_bytes)} · ${p.total_items.toLocaleString()} items` : `${p.total_items.toLocaleString()} items`;
  }
  if (p.state === "Cancelled") return "Stopped. Items already processed stay where they are.";
  if (p.state === "Failed") return `${p.errors.length} error${p.errors.length === 1 ? "" : "s"}`;
  const parts: string[] = [];
  if (p.total_bytes) parts.push(`${formatSize(p.done_bytes)} of ${formatSize(p.total_bytes)}`);
  else parts.push(`${p.done_items.toLocaleString()} of ${p.total_items.toLocaleString()}`);
  if (p.bytes_per_sec) parts.push(formatRate(p.bytes_per_sec));
  parts.push(etaText(p.eta_secs));
  return parts.join(" · ");
}

export function etaText(eta: number | null): string {
  return eta === null ? "estimating…" : eta <= 0 ? "almost done" : `${formatEta(eta)} left`;
}

export const isTerminal = (p: Progress | null) => !!p && (p.state === "Done" || p.state === "Cancelled" || p.state === "Failed");
