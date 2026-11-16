/**
 * Preview pane for the focused item in the active pane.
 *
 * Lazy by construction: it waits for the cursor to settle (holding an arrow key through
 * a folder must not fire a request per row), loads off the listing path, and drops
 * results for an item the user has already moved past.
 */
import { useEffect, useState, useSyncExternalStore } from "react";
import { getApi } from "../api";
import type { FileEntry } from "../api/types";
import { previewKind, typeInfo } from "../lib/fileTypes";
import { formatSize } from "../lib/format";
import { parent } from "../lib/path";
import { togglePreview, useApp } from "../state/app";
import { runtimeFor, type TabRuntime } from "../state/tabRuntime";
import { useThumbnail } from "../state/thumbs";
import * as W from "../lib/workspace";
import { FileIcon, I } from "./Icons";

const SETTLE_MS = 140;
const TEXT_BYTES = 16 * 1024;

export function PreviewPane() {
  const tabId = useApp((s) => W.activeTab(s.workspace)?.id);
  if (!tabId) return null;
  return <PreviewFor rt={runtimeFor(tabId)} />;
}

function useSettled<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

function PreviewFor({ rt }: { rt: TabRuntime }) {
  const snap = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  const sel = snap.selection;
  const focus = sel.focus ?? sel.first();
  const entry = focus !== null ? rt.entryAt(focus) : undefined;
  const path = focus !== null ? rt.pathAt(focus) : undefined;
  const settledPath = useSettled(path, SETTLE_MS);
  const settled = settledPath === path;

  return (
    <aside aria-label="Preview" className="flex w-80 shrink-0 flex-col border-l border-hairline bg-surface">
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-hairline pl-3.5 pr-1.5">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-ink-faint">Preview</span>
        <button className="btn-icon size-6" aria-label="Close preview" onClick={togglePreview}>
          <I.Close size={12} />
        </button>
      </div>
      {sel.size > 1 ? (
        <MultiSummary rt={rt} />
      ) : entry && path ? (
        <Details entry={entry} path={path} load={settled} />
      ) : (
        <div className="grid flex-1 place-items-center px-6 text-center text-xs text-ink-faint">Select a file to preview it.</div>
      )}
    </aside>
  );
}

function Details({ entry, path, load }: { entry: FileEntry; path: string; load: boolean }) {
  const info = typeInfo(entry);
  const kind = previewKind(entry);
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="p-4">
        <Visual entry={entry} path={path} kind={kind} load={load} icon={info.icon} />
        <h3 className="mt-4 break-words text-[13px] font-semibold leading-snug text-ink">{entry.name}</h3>
        <div className="mt-0.5 text-[11px] text-ink-muted">{info.label}</div>
      </div>
      <dl className="mx-4 space-y-2 border-t border-hairline py-3 text-[11px] tabular-nums">
        {entry.kind !== "Dir" && <Fact k="Size" v={`${formatSize(entry.size)}${entry.size >= 1024 ? ` (${entry.size.toLocaleString()} bytes)` : ""}`} />}
        <Fact k="Modified" v={formatFull(entry.modified)} />
        <Fact k="Created" v={formatFull(entry.created)} />
        <Fact k="Location" v={parent(path) ?? path} mono />
        <Fact k="Attributes" v={attrList(entry) || "—"} />
      </dl>
      {kind === "text" && <TextPreview path={path} load={load} />}
      {kind === "media" && (
        <p className="mx-4 rounded-md bg-base p-3 text-[11px] leading-relaxed text-ink-muted shadow-[inset_0_0_0_1px_var(--color-hairline)]">
          Duration, codec and resolution need the media metadata command, which the backend does not expose yet.
        </p>
      )}
    </div>
  );
}

function Visual({ entry, path, kind, load, icon }: { entry: FileEntry; path: string; kind: ReturnType<typeof previewKind>; load: boolean; icon: ReturnType<typeof typeInfo>["icon"] }) {
  // The OS thumbnail provider renders images and PDFs; ask only once the cursor settles.
  const thumb = useThumbnail(kind === "image" || kind === "pdf" ? path : undefined, 288, load, 0);
  return (
    <div className="grid aspect-[4/3] w-full place-items-center overflow-hidden rounded-lg bg-base shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      {thumb ? (
        <img src={thumb} alt={`Preview of ${entry.name}`} className="max-h-full max-w-full object-contain" style={{ animation: "fade-in var(--dur) var(--ease-out)" }} />
      ) : (
        <FileIcon kind={icon} size={64} />
      )}
    </div>
  );
}

function TextPreview({ path, load }: { path: string; load: boolean }) {
  const [state, setState] = useState<{ path: string; text: string | null; unsupported?: boolean } | null>(null);
  useEffect(() => {
    if (!load) return;
    let live = true;
    void getApi().then(async (api) => {
      if (!api.readText) {
        if (live) setState({ path, text: null, unsupported: true });
        return;
      }
      const text = await api.readText(path, TEXT_BYTES).catch(() => null);
      if (live) setState({ path, text });
    });
    return () => {
      live = false;
    };
  }, [path, load]);

  if (!state || state.path !== path) return <div className="mx-4 h-40 animate-pulse rounded-md bg-base" />;
  if (state.unsupported) return <p className="mx-4 text-[11px] text-ink-faint">Text preview is not available from this backend yet.</p>;
  if (state.text === null) return <p className="mx-4 text-[11px] text-ink-faint">Could not read this file.</p>;
  return (
    <pre className="mx-4 mb-4 max-h-[50vh] overflow-auto whitespace-pre-wrap break-words rounded-md bg-base p-3 font-mono text-[11px] leading-relaxed text-ink-muted shadow-[inset_0_0_0_1px_var(--color-hairline)] [user-select:text]">
      {state.text}
    </pre>
  );
}

function MultiSummary({ rt }: { rt: TabRuntime }) {
  const sel = rt.getSnapshot().selection;
  let files = 0;
  let dirs = 0;
  let bytes = 0;
  const capped = sel.size > 20_000;
  if (!capped) {
    for (const e of rt.selectedEntries(20_000)) {
      if (e.kind === "Dir") dirs++;
      else {
        files++;
        bytes += e.size;
      }
    }
  }
  return (
    <div className="p-4">
      <div className="grid aspect-[4/3] w-full place-items-center rounded-lg bg-base shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        <div className="text-center">
          <div className="text-3xl font-semibold tabular-nums text-ink">{sel.size.toLocaleString()}</div>
          <div className="text-xs text-ink-muted">items selected</div>
        </div>
      </div>
      {!capped && (
        <dl className="mt-4 space-y-2 text-[11px] tabular-nums">
          <Fact k="Files" v={files.toLocaleString()} />
          <Fact k="Folders" v={dirs.toLocaleString()} />
          <Fact k="Total size" v={`${formatSize(bytes)}${dirs ? " (excluding folders)" : ""}`} />
        </dl>
      )}
    </div>
  );
}

function Fact({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="flex gap-3">
      <dt className="w-20 shrink-0 text-ink-faint">{k}</dt>
      <dd className={`min-w-0 break-words text-ink-muted ${mono ? "font-mono text-[10.5px]" : ""}`}>{v}</dd>
    </div>
  );
}

function formatFull(ms: number): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function attrList(e: FileEntry): string {
  const a = e.attrs;
  return [a.readonly && "Read-only", a.hidden && "Hidden", a.system && "System", a.compressed && "Compressed", a.encrypted && "Encrypted", a.sparse && "Sparse", a.reparse && "Link"]
    .filter(Boolean)
    .join(", ");
}

