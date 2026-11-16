/**
 * Modal and floating layers: the command palette (Ctrl+P files, Ctrl+Shift+P commands),
 * the file context menu, confirmation dialogs and transient notices.
 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { getApi } from "../api";
import type { QuerySummary, SearchHit } from "../api/types";
import { typeInfo } from "../lib/fileTypes";
import { highlightSegments } from "../lib/highlight";
import { rankCommands } from "../lib/palette";
import { parent } from "../lib/path";
import { askConfirm, navigate, openMenu, openPalette, revealInPane, runtimeOf, useApp } from "../state/app";
import { COMMANDS, keys, useCommand, useCommands } from "../state/commands";
import { FileIcon, I } from "./Icons";
import { Chord } from "./Kbd";
import { Menu, type MenuEntry, Popover } from "./Popover";

/* ───────────────────────────── Command palette ───────────────────────────── */

const RECENT_KEY = "fulcrum.recentCommands";
function recentCommands(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]") as string[];
  } catch {
    return [];
  }
}
function pushRecent(id: string) {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify([id, ...recentCommands().filter((x) => x !== id)].slice(0, 8)));
  } catch {
    /* non-essential */
  }
}

export function CommandPalette() {
  const mode = useApp((s) => s.palette);
  if (!mode) return null;
  return <PaletteDialog initial={mode} />;
}

type Row = { kind: "cmd"; id: string; title: string; category: string; runs: [number, number][] } | { kind: "file"; hit: SearchHit };

function PaletteDialog({ initial }: { initial: "files" | "commands" }) {
  // VS Code convention: a leading ">" means commands; deleting it goes back to files.
  const [text, setText] = useState(initial === "commands" ? ">" : "");
  const [active, setActive] = useState(0);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [summary, setSummary] = useState<QuerySummary | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  useSyncExternalStore(keys.subscribe, () => keys.getKeymap());

  const isCommands = text.startsWith(">");
  const query = isCommands ? text.slice(1).trim() : text.trim();
  const close = () => openPalette(null);

  useLayoutEffect(() => {
    const el = inputRef.current;
    el?.focus();
    el?.setSelectionRange(el.value.length, el.value.length);
  }, []);

  // File search goes to Rust; stale responses are discarded by generation.
  const gen = useRef(0);
  useEffect(() => {
    if (isCommands || !query) {
      setHits([]);
      setSummary(null);
      return;
    }
    const g = ++gen.current;
    const t = setTimeout(async () => {
      const api = await getApi();
      const got: SearchHit[] = [];
      try {
        const sum = await api.search(
          {
            text: query,
            mode: "Fuzzy",
            case: "Smart",
            scope: "All",
            filters: { kinds: [], extensions: [], size: null, modified: null, created: null, include_hidden: false, include_system: false },
            sort: { key: "Relevance", desc: true, dirs_first: false },
            limit: 60,
            offset: 0,
            match_full_path: false,
          },
          // Batches travel on a separate channel from the call's own response and can land
          // after it resolves, so each one must update state itself. Copying matters:
          // React skips the render if handed the same array it already holds.
          (batch) => {
            if (g !== gen.current) return;
            got.push(...batch);
            setHits(got.slice());
          },
        );
        if (g !== gen.current) return;
        setHits(got.slice());
        setSummary(sum);
        setActive(0);
      } catch {
        if (g === gen.current) {
          setHits([]);
          setSummary(null); // never show the previous query's match count beside an empty list
        }
      }
    }, 40);
    return () => clearTimeout(t);
  }, [query, isCommands]);

  const rows: Row[] = useMemo(() => {
    if (isCommands) {
      return rankCommands(query, COMMANDS, recentCommands()).map((r) => ({ kind: "cmd", id: r.cmd.id, title: r.cmd.title, category: r.cmd.category, runs: r.runs }));
    }
    return hits.map((hit) => ({ kind: "file", hit }));
  }, [isCommands, query, hits]);

  useEffect(() => setActive(0), [isCommands, query]);

  useEffect(() => {
    const el = listRef.current?.querySelector(`[data-row="${active}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const run = (i: number) => {
    const row = rows[i];
    if (!row) return;
    close();
    if (row.kind === "cmd") {
      pushRecent(row.id);
      // Defer so the palette's handlers are gone before the command runs.
      setTimeout(() => keys.run(row.id, { inInput: false, source: "palette" }), 0);
    } else if (row.hit.entry.kind === "Dir") navigate(row.hit.path);
    else revealInPane(row.hit.path);
  };

  useCommands({
    "list.down": () => setActive((a) => Math.min(rows.length - 1, a + 1)),
    "list.up": () => setActive((a) => Math.max(0, a - 1)),
    "list.pageDown": () => setActive((a) => Math.min(rows.length - 1, a + 8)),
    "list.pageUp": () => setActive((a) => Math.max(0, a - 8)),
    "list.open": () => run(active),
    "ui.cancel": () => close(),
    // Pressing the palette chord again switches mode rather than stacking a second palette.
    "palette.files": () => setText((t) => (t.startsWith(">") ? t.slice(1) : t)),
    "palette.commands": () => setText((t) => (t.startsWith(">") ? t : `>${t}`)),
  });

  return createPortal(
    <div className="fixed inset-0 z-50 flex justify-center bg-black/40 pt-[12vh]" style={{ animation: "fade-in var(--dur) var(--ease-out)" }} onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div role="dialog" aria-label="Command palette" className="popover flex h-fit max-h-[60vh] w-[min(640px,92vw)] flex-col overflow-hidden">
        <div className="flex items-center gap-2.5 border-b border-hairline px-3.5">
          {isCommands ? <I.Command size={15} className="text-accent-300" /> : <I.Search size={15} className="text-accent-300" />}
          <input
            ref={inputRef}
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder={isCommands ? "Type a command…" : "Jump to a file or folder anywhere…  (type > for commands)"}
            spellCheck={false}
            aria-label="Palette query"
            className="h-12 min-w-0 flex-1 bg-transparent text-[14px] text-ink outline-none placeholder:text-ink-faint"
          />
          <kbd className="kbd">Esc</kbd>
        </div>
        <div ref={listRef} role="listbox" className="min-h-0 flex-1 overflow-y-auto p-1.5">
          {rows.map((row, i) => (
            <div
              key={row.kind === "cmd" ? row.id : row.hit.path}
              data-row={i}
              role="option"
              aria-selected={i === active}
              data-active={i === active}
              onMouseMove={() => i !== active && setActive(i)}
              onClick={() => run(i)}
              className="menu-item h-auto min-h-9 py-1.5"
            >
              {row.kind === "cmd" ? <CommandRow row={row} /> : <FileRow hit={row.hit} />}
            </div>
          ))}
          {rows.length === 0 && (
            <div className="px-3 py-6 text-center text-xs text-ink-faint">{isCommands ? "No matching commands" : query ? "No matches" : "Start typing to search every indexed file"}</div>
          )}
        </div>
        {!isCommands && summary && (
          <div className="flex items-center gap-2 border-t border-hairline px-3.5 py-2 text-[11px] text-ink-faint">
            <span className="tabular-nums">
              {summary.total_matched.toLocaleString()} matches · {(summary.elapsed_micros / 1000).toFixed(1)} ms
            </span>
            {summary.degraded && (
              <span className="ml-auto flex items-center gap-1 text-warn">
                <I.Warn size={12} /> Partial: instant search is off
              </span>
            )}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}

function Highlighted({ text, runs }: { text: string; runs: [number, number][] }) {
  let pos = 0;
  const out: React.ReactNode[] = [];
  runs.forEach(([a, b], i) => {
    if (a > pos) out.push(text.slice(pos, a));
    out.push(
      <mark key={i} className="match">
        {text.slice(a, b)}
      </mark>,
    );
    pos = b;
  });
  out.push(text.slice(pos));
  return <>{out}</>;
}

function CommandRow({ row }: { row: Extract<Row, { kind: "cmd" }> }) {
  const chord = keys.chordsFor(row.id)[0];
  return (
    <>
      <span className="w-20 shrink-0 text-[11px] text-ink-faint">{row.category}</span>
      <span className="min-w-0 flex-1 truncate">
        <Highlighted text={row.title} runs={row.runs} />
      </span>
      {chord && <Chord chord={chord} />}
    </>
  );
}

function FileRow({ hit }: { hit: SearchHit }) {
  const info = typeInfo(hit.entry);
  return (
    <>
      <FileIcon kind={info.icon} />
      <span className="min-w-0 flex-1">
        <span className="block truncate">
          {/* Ranges come from Rust as UTF-8 byte offsets; never re-matched here. */}
          {highlightSegments(hit.entry.name, hit.match_ranges).map((s, i) =>
            s.match ? (
              <mark key={i} className="match">
                {s.text}
              </mark>
            ) : (
              <span key={i}>{s.text}</span>
            ),
          )}
        </span>
        <span className="block truncate text-[11px] text-ink-faint">{parent(hit.path)}</span>
      </span>
      <span className="shrink-0 text-[11px] text-ink-faint">{info.label}</span>
    </>
  );
}

/* ───────────────────────────── File context menu ───────────────────────────── */

/** Every item is a registry command, so the menu shows (and honours) rebound chords. */
export function ContextMenu() {
  const menu = useApp((s) => s.menu);
  const clipboard = useApp((s) => s.clipboard);
  const split = useApp((s) => s.workspace.layout !== "Single");
  useSyncExternalStore(keys.subscribe, () => keys.getKeymap());
  if (!menu) return null;

  const rt = runtimeOf(menu.pane);
  const sel = rt?.getSnapshot().selection;
  const count = sel?.size ?? 0;
  const focusEntry = sel?.focus !== null && sel?.focus !== undefined ? rt?.entryAt(sel.focus) : undefined;
  const single = count === 1;
  const close = () => openMenu(null);

  const item = (id: string, label: string, icon?: React.ReactNode, opts: { disabled?: boolean; danger?: boolean } = {}): MenuEntry => ({
    id,
    label,
    icon,
    disabled: opts.disabled,
    danger: opts.danger,
    hint: keys.chordsFor(id)[0] ? <Chord chord={keys.chordsFor(id)[0]!} /> : undefined,
    // Deferred: the menu's own list.open/arrow handlers must be unregistered (it closes
    // first) before the command runs, or "Open" would re-enter the menu's handler.
    run: () => setTimeout(() => keys.run(id, { inInput: false, source: "menu" }), 0),
  });

  const items: MenuEntry[] = menu.onRow
    ? [
        item("list.open", "Open", <I.External size={14} />, { disabled: !single }),
        ...(focusEntry?.kind === "Dir" ? [item("list.openNewTab", "Open in new tab", <I.Plus size={14} />)] : []),
        ...(rt?.getSnapshot().mode === "search" ? [{ id: "reveal-here", label: "Open file location", icon: <I.Forward size={14} />, run: () => {
          const p = rt.pathAt(sel?.focus ?? -1);
          if (p) revealInPane(p, menu.pane);
        } } as MenuEntry] : []),
        "separator",
        item("file.cut", "Cut", <I.Cut size={14} />),
        item("file.copy", "Copy", <I.Copy size={14} />),
        item("file.copyPath", "Copy path", <I.Link size={14} />),
        ...(split
          ? [item("pane.copyToOther", "Copy to other pane", <I.SplitV size={14} />), item("pane.moveToOther", "Move to other pane", <I.Forward size={14} />)]
          : []),
        "separator",
        item("file.rename", "Rename", <I.Rename size={14} />, { disabled: !single }),
        ...(focusEntry?.kind === "Dir" ? [item("file.pin", "Pin to sidebar", <I.Pin size={14} />)] : []),
        item("file.properties", "Properties", <I.Eye size={14} />),
        "separator",
        item("file.trash", count > 1 ? `Move ${count.toLocaleString()} items to Recycle Bin` : "Move to Recycle Bin", <I.Trash size={14} />, { danger: true }),
      ]
    : [
        item("file.newFolder", "New folder", <I.FolderPlus size={14} />),
        item("file.paste", clipboard ? `Paste ${clipboard.paths.length.toLocaleString()} item${clipboard.paths.length === 1 ? "" : "s"}` : "Paste", <I.Paste size={14} />, { disabled: !clipboard }),
        "separator",
        item("nav.refresh", "Refresh", <I.Refresh size={14} />),
        item("list.selectAll", "Select all", <I.Check size={14} />),
        item("view.hidden", "Show hidden files", <I.Eye size={14} />),
        "separator",
        item("file.copyPath", "Copy folder path", <I.Link size={14} />),
        item("file.pin", "Pin this folder", <I.Pin size={14} />),
      ];

  return (
    <Popover anchor={{ x: menu.x, y: menu.y }} onClose={close}>
      <Menu items={items} onClose={close} />
    </Popover>
  );
}

/* ───────────────────────────── Confirm dialog ───────────────────────────── */

export function ConfirmDialog() {
  const c = useApp((s) => s.confirm);
  const okRef = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => okRef.current?.focus(), [c]);
  useCommand("ui.cancel", () => askConfirm(null), !!c);
  if (!c) return null;
  const confirm = () => {
    askConfirm(null);
    c.onConfirm();
  };
  return (
    <Modal onClose={() => askConfirm(null)} label={c.title}>
      <div className="w-[400px] p-5">
        <div className="flex gap-3">
          <div className={`grid size-9 shrink-0 place-items-center rounded-full ${c.danger ? "bg-[color-mix(in_oklab,var(--color-danger)_15%,transparent)] text-danger" : "bg-select text-accent-300"}`}>
            {c.danger ? <I.Warn size={16} /> : <I.Trash size={16} />}
          </div>
          <div>
            <h2 className="text-[14px] font-semibold text-ink">{c.title}</h2>
            <p className="mt-1 text-xs leading-relaxed text-ink-muted">{c.body}</p>
          </div>
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button className="btn" onClick={() => askConfirm(null)}>
            Cancel
          </button>
          {/* Enter is a native button press here, not a shortcut: focus starts on it. */}
          <button ref={okRef} className={`btn ${c.danger ? "bg-[#b91c1c]! text-white hover:bg-[#dc2626]!" : "btn-primary"}`} onClick={confirm}>
            {c.confirmLabel}
          </button>
        </div>
      </div>
    </Modal>
  );
}

export function Modal({ children, onClose, label, wide }: { children: React.ReactNode; onClose: () => void; label: string; wide?: boolean }) {
  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/50" style={{ animation: "fade-in var(--dur) var(--ease-out)" }} onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-modal aria-label={label} className={`popover overflow-hidden ${wide ? "w-[min(860px,94vw)]" : ""}`}>
        {children}
      </div>
    </div>,
    document.body,
  );
}

/* ───────────────────────────── Notices ───────────────────────────── */

export function Notices() {
  const notices = useApp((s) => s.notices);
  return (
    <div aria-live="polite" className="pointer-events-none fixed bottom-10 left-1/2 z-40 flex -translate-x-1/2 flex-col items-center gap-1.5">
      {notices.map((n) => (
        <div
          key={n.id}
          className={`flex items-center gap-2 rounded-full px-3.5 py-1.5 text-xs shadow-[var(--shadow-popover)] ${n.tone === "error" ? "bg-[#3a1620] text-[#fecaca]" : "bg-overlay text-ink"}`}
          style={{ animation: "slide-up var(--dur) var(--ease-out)" }}
        >
          {n.tone === "error" ? <I.Warn size={13} className="text-danger" /> : <I.Check size={13} className="text-accent-300" />}
          {n.text}
        </div>
      ))}
    </div>
  );
}

