/**
 * The icon set. Drawn on one 16px grid with one stroke weight so toolbars read as a
 * single family; no icon font, no dependency, nothing to load.
 */
import { memo, type SVGProps } from "react";
import type { IconKind } from "../lib/fileTypes";

type P = SVGProps<SVGSVGElement> & { size?: number };

function Svg({ size = 16, children, ...rest }: P) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...rest}
    >
      {children}
    </svg>
  );
}

export const I = {
  Back: (p: P) => <Svg {...p}><path d="M10 3.5 5.5 8l4.5 4.5" /></Svg>,
  Forward: (p: P) => <Svg {...p}><path d="M6 3.5 10.5 8 6 12.5" /></Svg>,
  Up: (p: P) => <Svg {...p}><path d="M3.5 10 8 5.5l4.5 4.5" /></Svg>,
  Chevron: (p: P) => <Svg {...p}><path d="m6.5 4.5 3.5 3.5-3.5 3.5" /></Svg>,
  ChevronDown: (p: P) => <Svg {...p}><path d="m4.5 6.5 3.5 3.5 3.5-3.5" /></Svg>,
  Refresh: (p: P) => <Svg {...p}><path d="M13 8a5 5 0 1 1-1.6-3.7M13 2.8v2.6h-2.6" /></Svg>,
  Search: (p: P) => <Svg {...p}><circle cx="7" cy="7" r="4.2" /><path d="m10.2 10.2 3.3 3.3" /></Svg>,
  Close: (p: P) => <Svg {...p}><path d="m4.5 4.5 7 7m0-7-7 7" /></Svg>,
  Plus: (p: P) => <Svg {...p}><path d="M8 3.5v9M3.5 8h9" /></Svg>,
  Details: (p: P) => <Svg {...p}><path d="M2.5 4h11M2.5 8h11M2.5 12h11" /></Svg>,
  List: (p: P) => <Svg {...p}><path d="M2.5 4h4M2.5 8h4M2.5 12h4M9.5 4h4M9.5 8h4M9.5 12h4" /></Svg>,
  Grid: (p: P) => (
    <Svg {...p}>
      <rect x="2.5" y="2.5" width="4.2" height="4.2" rx="1" />
      <rect x="9.3" y="2.5" width="4.2" height="4.2" rx="1" />
      <rect x="2.5" y="9.3" width="4.2" height="4.2" rx="1" />
      <rect x="9.3" y="9.3" width="4.2" height="4.2" rx="1" />
    </Svg>
  ),
  Tiles: (p: P) => (
    <Svg {...p}>
      <rect x="2.5" y="2.5" width="3.5" height="3.5" rx="0.8" />
      <rect x="2.5" y="10" width="3.5" height="3.5" rx="0.8" />
      <path d="M8 3.5h5.5M8 5.5h3.5M8 11h5.5M8 13h3.5" />
    </Svg>
  ),
  SplitV: (p: P) => <Svg {...p}><rect x="2" y="2.5" width="12" height="11" rx="1.5" /><path d="M8 2.5v11" /></Svg>,
  SplitH: (p: P) => <Svg {...p}><rect x="2" y="2.5" width="12" height="11" rx="1.5" /><path d="M2 8h12" /></Svg>,
  Single: (p: P) => <Svg {...p}><rect x="2" y="2.5" width="12" height="11" rx="1.5" /></Svg>,
  Preview: (p: P) => <Svg {...p}><rect x="2" y="2.5" width="12" height="11" rx="1.5" /><path d="M10 2.5v11" /><path d="M11.5 5.5h1M11.5 7.5h1" /></Svg>,
  Sidebar: (p: P) => <Svg {...p}><rect x="2" y="2.5" width="12" height="11" rx="1.5" /><path d="M6 2.5v11" /></Svg>,
  Settings: (p: P) => (
    <Svg {...p}>
      <circle cx="8" cy="8" r="2" />
      <path d="M8 1.8v1.6M8 12.6v1.6M14.2 8h-1.6M3.4 8H1.8M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1M12.4 12.4l-1.1-1.1M4.7 4.7 3.6 3.6" />
    </Svg>
  ),
  Command: (p: P) => <Svg {...p}><path d="M5.5 10.5h5v-5h-5zM5.5 5.5a2 2 0 1 0-2-2v2zM10.5 5.5a2 2 0 1 1 2-2v2zM5.5 10.5a2 2 0 1 1-2 2v-2zM10.5 10.5a2 2 0 1 0 2 2v-2z" /></Svg>,
  Pin: (p: P) => <Svg {...p}><path d="M9.5 2.5 13.5 6.5 11 7.5 8.5 10l.5 3-6.5-6.5 3 .5L8 4.5z" /><path d="m5 11-2.5 2.5" /></Svg>,
  Home: (p: P) => <Svg {...p}><path d="M2.5 7 8 2.5 13.5 7v6.5h-4v-4h-3v4h-4z" /></Svg>,
  Desktop: (p: P) => <Svg {...p}><rect x="2" y="3" width="12" height="8" rx="1.2" /><path d="M6 13.5h4M8 11v2.5" /></Svg>,
  Download: (p: P) => <Svg {...p}><path d="M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10" /></Svg>,
  Doc: (p: P) => <Svg {...p}><path d="M4 1.8h5.2L12.5 5v9.2H4z" /><path d="M9 1.8V5h3.5M6 8.5h4.5M6 11h4.5" /></Svg>,
  Picture: (p: P) => <Svg {...p}><rect x="2" y="3" width="12" height="10" rx="1.5" /><circle cx="6" cy="6.5" r="1.2" /><path d="m2.5 12 3.5-3.5 2.5 2.5 2-2 3 3" /></Svg>,
  Music: (p: P) => <Svg {...p}><path d="M6 12V3.5l7-1.5V10.5" /><circle cx="4.5" cy="12" r="1.6" /><circle cx="11.5" cy="10.5" r="1.6" /></Svg>,
  Video: (p: P) => <Svg {...p}><rect x="2" y="4" width="9" height="8" rx="1.3" /><path d="m11 7 3-2v6l-3-2" /></Svg>,
  Drive: (p: P) => <Svg {...p}><rect x="2" y="4.5" width="12" height="7" rx="1.5" /><path d="M2 8.5h12" /><circle cx="11.5" cy="10" r=".5" fill="currentColor" /></Svg>,
  Usb: (p: P) => <Svg {...p}><rect x="4.5" y="6" width="7" height="8" rx="1.2" /><path d="M6 6V2.5h4V6M7 4h.01M9 4h.01" /></Svg>,
  Copy: (p: P) => <Svg {...p}><rect x="5" y="5" width="8.5" height="8.5" rx="1.5" /><path d="M11 5V3.5A1 1 0 0 0 10 2.5H3.5a1 1 0 0 0-1 1V10a1 1 0 0 0 1 1H5" /></Svg>,
  Cut: (p: P) => <Svg {...p}><circle cx="4.5" cy="11.5" r="2" /><circle cx="11.5" cy="11.5" r="2" /><path d="M6 10 12 2.5M10 10 4 2.5" /></Svg>,
  Paste: (p: P) => <Svg {...p}><rect x="3" y="3" width="10" height="11" rx="1.5" /><path d="M6 3V2h4v1" /></Svg>,
  Trash: (p: P) => <Svg {...p}><path d="M2.5 4.5h11M6 4.5V3h4v1.5M4 4.5l.7 9h6.6l.7-9" /></Svg>,
  Rename: (p: P) => <Svg {...p}><path d="M10.5 2.5 13.5 5.5 6 13H3v-3z" /></Svg>,
  FolderPlus: (p: P) => <Svg {...p}><path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" /><path d="M8 7v4M6 9h4" /></Svg>,
  Link: (p: P) => <Svg {...p}><path d="M7 9a2.5 2.5 0 0 0 3.5 0l2-2A2.5 2.5 0 0 0 9 3.5l-.5.5M9 7a2.5 2.5 0 0 0-3.5 0l-2 2A2.5 2.5 0 0 0 7 12.5l.5-.5" /></Svg>,
  Pause: (p: P) => <Svg {...p}><path d="M5.5 3.5v9M10.5 3.5v9" /></Svg>,
  Play: (p: P) => <Svg {...p}><path d="M5 3.2v9.6L12.5 8z" /></Svg>,
  Stop: (p: P) => <Svg {...p}><rect x="4" y="4" width="8" height="8" rx="1.2" /></Svg>,
  Bolt: (p: P) => <Svg {...p}><path d="M9 1.8 3.5 9h4L7 14.2 12.5 7h-4z" /></Svg>,
  Warn: (p: P) => <Svg {...p}><path d="M8 2.2 14.3 13H1.7z" /><path d="M8 6.5v3M8 11.3h.01" /></Svg>,
  Check: (p: P) => <Svg {...p}><path d="m3.5 8.5 3 3 6-7" /></Svg>,
  Eye: (p: P) => <Svg {...p}><path d="M1.8 8S4 3.5 8 3.5 14.2 8 14.2 8 12 12.5 8 12.5 1.8 8 1.8 8z" /><circle cx="8" cy="8" r="2" /></Svg>,
  External: (p: P) => <Svg {...p}><path d="M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5V13a.5.5 0 0 1-.5.5h-8A.5.5 0 0 1 3 13V5a.5.5 0 0 1 .5-.5H7" /></Svg>,
  Sort: (p: P) => <Svg {...p}><path d="M5 3v10M2.5 10.5 5 13l2.5-2.5M11 13V3M8.5 5.5 11 3l2.5 2.5" /></Svg>,
  ArrowUpSmall: (p: P) => <Svg {...p}><path d="M8 12V4.5M5 7.5l3-3 3 3" /></Svg>,
  ArrowDownSmall: (p: P) => <Svg {...p}><path d="M8 4v7.5M5 8.5l3 3 3-3" /></Svg>,
  Keyboard: (p: P) => <Svg {...p}><rect x="1.8" y="4" width="12.4" height="8" rx="1.5" /><path d="M4.5 6.5h.01M7 6.5h.01M9.5 6.5h.01M12 6.5h.01M5 9.5h6" /></Svg>,
  Minimize: (p: P) => <Svg {...p}><path d="M3.5 8h9" /></Svg>,
  Maximize: (p: P) => <Svg {...p}><rect x="3.5" y="3.5" width="9" height="9" rx="1" /></Svg>,
};

/** The Fulcrum mark: a lever balanced on a fulcrum. */
export function Logo({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 20 20" aria-hidden>
      <defs>
        <linearGradient id="fx-logo" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="var(--color-accent-300)" />
          <stop offset="1" stopColor="var(--color-accent-600)" />
        </linearGradient>
      </defs>
      <rect x="1" y="1" width="18" height="18" rx="5" fill="url(#fx-logo)" />
      <path d="M4.5 9.2 15.5 6.6" stroke="white" strokeWidth="1.8" strokeLinecap="round" />
      <path d="M10 8.2 13 14H7z" fill="white" fillOpacity=".92" />
    </svg>
  );
}

const TYPE_COLOR: Record<IconKind, string> = {
  folder: "var(--color-accent-400)",
  image: "var(--color-type-image)",
  video: "var(--color-type-video)",
  audio: "var(--color-type-audio)",
  archive: "var(--color-type-archive)",
  code: "var(--color-type-code)",
  doc: "var(--color-type-doc)",
  sheet: "var(--color-type-sheet)",
  slides: "var(--color-type-slides)",
  pdf: "var(--color-type-pdf)",
  text: "var(--color-type-text)",
  exe: "var(--color-type-exe)",
  link: "var(--color-ink-muted)",
  file: "var(--color-ink-faint)",
};

const BADGE: Partial<Record<IconKind, string>> = {
  image: "M5.5 11.5 7.5 9l1.3 1.5L10 9.2l1.5 2.3z",
  video: "M7 8.2v3.6l3-1.8z",
  audio: "M7.5 11.8V8.5l3-.7v3.2",
  archive: "M8 5v1M8 7v1M8 9v1",
  code: "m6.8 8.5-1.3 1.3 1.3 1.3M9.2 8.5l1.3 1.3-1.3 1.3",
  sheet: "M5.5 8.5h5M5.5 10.5h5M8 7.5v4.5",
  doc: "M5.5 8.5h5M5.5 10.5h3.5",
  text: "M5.5 8.5h5M5.5 10.5h5M5.5 12.5h3",
  pdf: "M5.8 11.5c1.5-1 2.8-3.3 2.3-4-.6-.6-.8 2.2 2.4 3.6",
  slides: "M5.5 8h5v3.5h-5z",
  exe: "M5.5 8h5v4h-5zM5.5 9.3h5",
  link: "M6 11.5 10 7.5M7.5 7.5H10V10",
};

/**
 * The per-row type glyph. Memoised on its one prop: it renders for every visible row on
 * every scroll frame, so it must be the cheapest thing in the row.
 */
export const FileIcon = memo(function FileIcon({ kind, size = 16 }: { kind: IconKind; size?: number }) {
  const color = TYPE_COLOR[kind];
  if (kind === "folder") {
    return (
      <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden className="row-icon shrink-0">
        <path d="M1.5 4a1.2 1.2 0 0 1 1.2-1.2h3.4l1.6 1.6h5.6A1.2 1.2 0 0 1 14.5 5.6v6.6a1.2 1.2 0 0 1-1.2 1.2H2.7a1.2 1.2 0 0 1-1.2-1.2z" fill={color} fillOpacity=".22" stroke={color} strokeWidth="1.1" />
        <path d="M1.5 6.2h13" stroke={color} strokeOpacity=".55" strokeWidth="1" />
      </svg>
    );
  }
  const badge = BADGE[kind];
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden className="row-icon shrink-0" fill="none" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3.5 1.8h5.6l3.4 3.4v8.4a.8.8 0 0 1-.8.8H3.5a.8.8 0 0 1-.8-.8V2.6a.8.8 0 0 1 .8-.8z" fill={color} fillOpacity=".1" stroke={color} strokeWidth="1.1" />
      <path d="M9 1.8v3.5h3.5" stroke={color} strokeWidth="1.1" />
      {badge && <path d={badge} stroke={color} strokeWidth="1.1" />}
    </svg>
  );
});
