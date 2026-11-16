/** Display formatting for file metadata. */

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/**
 * Formats a byte count the way a file manager should: base-1024, three significant
 * figures, and no decimal on bytes.
 */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes === 0) return "0 B";
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), UNITS.length - 1);
  const value = bytes / 1024 ** i;
  // Keep the column narrow and stable: no decimals for bytes, one above 100.
  const decimals = i === 0 ? 0 : value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals)} ${UNITS[i]}`;
}

// `toLocaleDateString` builds a fresh Intl formatter on every call, which is the single
// most expensive thing a row did while scrolling. Formatters are built once.
const TIME_FMT = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
const DATE_FMT = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "short", day: "2-digit" });

/**
 * Formats a timestamp. `0` is the contract's "unknown" value and must not render as
 * 1 January 1970.
 */
export function formatDate(millis: number, now: number = Date.now()): string {
  if (!millis) return "—";
  const d = new Date(millis);
  const n = new Date(now);
  const sameDay = d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() && d.getDate() === n.getDate();
  return sameDay ? TIME_FMT.format(d) : DATE_FMT.format(d);
}

/** Splits a path into breadcrumb segments, handling both separators and UNC roots. */
export function breadcrumbs(path: string): { label: string; path: string }[] {
  if (!path) return [];
  const isUnc = path.startsWith("\\\\");
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(/[/\\]+/).filter(Boolean);

  const out: { label: string; path: string }[] = [];
  let acc = isUnc ? "\\\\" : path.startsWith("/") ? "/" : "";

  parts.forEach((part, i) => {
    acc = i === 0 ? acc + part : acc + sep + part;
    out.push({ label: part, path: acc });
  });
  return out;
}

/** Formats a transfer rate for the progress detail line. */
export function formatRate(bytesPerSec: number): string {
  if (!bytesPerSec) return "—";
  return `${formatSize(bytesPerSec)}/s`;
}

/** Formats a duration as the compact form used next to progress bars. */
export function formatEta(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "—";
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}
