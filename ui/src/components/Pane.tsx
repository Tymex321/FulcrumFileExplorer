/**
 * One pane: tab strip, toolbar (history, breadcrumb, search, view), the file view, and a
 * status line. Two of these side by side or stacked make the dual-pane layout; each has
 * its own tabs and history (see `lib/workspace.ts`).
 */
import { type DragEvent as RDragEvent, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Sort, SortKey, ViewMode } from "../api/types";
import { DRAG_MIME, decodePayload, dropOperation, dropRejection } from "../lib/dnd";
import { breadcrumbs, formatSize } from "../lib/format";
import { basename, normalize } from "../lib/path";
import * as W from "../lib/workspace";
import {
  dropPaths,
  enableInstantSearch,
  listOptionsFor,
  navigate,
  type PaneId,
  setEditingPath,
  updateWorkspace,
  useApp,
  goUp,
} from "../state/app";
import { useCommand } from "../state/commands";
import { activeDrag, activeTabDrag, setActiveTabDrag } from "../state/drag";
import {
  type DateChip,
  type KindChip,
  runtimeFor,
  type SearchParams,
  type SizeChip,
  type TabRuntime,
  type TypeChip,
} from "../state/tabRuntime";
import { FileView } from "./FileView";
import { FileIcon, I } from "./Icons";
import { CommandChord, useTitle } from "./Kbd";
import { Menu, type MenuEntry, Popover, usePopover } from "./Popover";

const TAB_MIME = "application/x-fulcrum-tab";

export function Pane({ pane }: { pane: PaneId }) {
  const paneState = useApp((s) => s.workspace.panes[pane]);
  const isActive = useApp((s) => s.workspace.activePane === pane);
  const split = useApp((s) => s.workspace.layout !== "Single");
  const settings = useApp((s) => s.settings);
  const tab = paneState.tabs[paneState.active];
  const rt = tab ? runtimeFor(tab.id) : null;

  // Load whenever the tab's folder, sort or list options change — but not when merely
  // switching back to a tab whose runtime already holds that listing.
  const options = tab ? listOptionsFor(tab, settings) : null;
  const optionsKey = JSON.stringify(options);
  useEffect(() => {
    if (!tab || !rt || !options || !settings) return;
    const cur = rt.current;
    if (cur && normalize(cur.path) === normalize(tab.path) && JSON.stringify(cur.options) === optionsKey) return;
    void rt.load({ path: tab.path, options });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab?.id, tab?.path, optionsKey, !!settings]);

  if (!tab || !rt) return null;

  return (
    <section
      data-pane-active={isActive}
      aria-label={`Pane ${pane + 1}`}
      className="relative flex min-h-0 min-w-0 flex-1 flex-col bg-base"
      onMouseDownCapture={() => !isActive && updateWorkspace((ws) => W.focusPane(ws, pane))}
    >
      {split && <div className={`absolute inset-x-0 top-0 z-10 h-px transition-fast ${isActive ? "bg-accent-500" : "bg-transparent"}`} />}
      <TabStrip pane={pane} tabs={paneState.tabs} active={paneState.active} />
      <Toolbar pane={pane} tab={tab} rt={rt} />
      <SearchFilters rt={rt} />
      <FileView pane={pane} tab={tab} rt={rt} active={isActive} />
      <StatusBar rt={rt} />
    </section>
  );
}

/* ───────────────────────────── Tabs ───────────────────────────── */

function TabStrip({ pane, tabs, active }: { pane: PaneId; tabs: W.Tab[]; active: number }) {
  const [dropAt, setDropAt] = useState<number | null>(null);
  const newTabTitle = useTitle("New tab", "tab.new");

  const slotFor = (e: RDragEvent, index: number) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return e.clientX < r.left + r.width / 2 ? index : index + 1;
  };

  const onDragOverTab = (e: RDragEvent, index: number) => {
    if (e.dataTransfer.types.includes(TAB_MIME)) {
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      setDropAt(slotFor(e, index));
      return;
    }
    // Files dragged over a tab switch to it after a beat, like a browser.
    if (e.dataTransfer.types.includes(DRAG_MIME)) {
      const t = tabs[index];
      const drag = activeDrag();
      if (!t || !drag) return;
      const op = dropOperation(drag.paths, t.path, { ctrl: e.ctrlKey, shift: e.shiftKey });
      if (dropRejection(drag.paths, t.path, op)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = op === "Copy" ? "copy" : "move";
      setDropAt(-1 - index);
    }
  };

  const onDrop = (e: RDragEvent, index: number) => {
    e.preventDefault();
    setDropAt(null);
    const td = activeTabDrag();
    if (e.dataTransfer.types.includes(TAB_MIME) && td) {
      const slot = slotFor(e, index);
      setActiveTabDrag(null);
      if (td.pane === pane) updateWorkspace((ws) => W.moveTab(ws, pane, td.index, slot > td.index ? slot - 1 : slot));
      else updateWorkspace((ws) => W.moveTabToPane(ws, td.pane, td.index, pane, slot));
      return;
    }
    const payload = decodePayload(e.dataTransfer.getData(DRAG_MIME));
    const t = tabs[index];
    if (payload && t) dropPaths(payload.paths, t.path, { ctrl: e.ctrlKey, shift: e.shiftKey });
  };

  return (
    <div
      role="tablist"
      className="flex h-9 shrink-0 items-end gap-0.5 overflow-x-auto border-b border-hairline bg-surface px-1.5 [scrollbar-width:none]"
      onDoubleClick={(e) => e.target === e.currentTarget && updateWorkspace((ws) => W.openTab(ws, pane, W.activeTab(ws, pane)?.path ?? "C:\\"))}
      onDragLeave={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && setDropAt(null)}
    >
      {tabs.map((t, i) => {
        const selected = i === active;
        const label = basename(t.path) || t.path;
        return (
          <div
            key={t.id}
            role="tab"
            aria-selected={selected}
            draggable
            title={t.path}
            onDragStart={(e) => {
              setActiveTabDrag({ pane, index: i });
              e.dataTransfer.setData(TAB_MIME, t.id);
              e.dataTransfer.effectAllowed = "move";
            }}
            onDragEnd={() => {
              setActiveTabDrag(null);
              setDropAt(null);
            }}
            onDragOver={(e) => onDragOverTab(e, i)}
            onDrop={(e) => onDrop(e, i)}
            onMouseDown={(e) => {
              if (e.button === 0) updateWorkspace((ws) => W.activateTab(ws, pane, i));
            }}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                updateWorkspace((ws) => W.closeTab(ws, pane, i));
              }
            }}
            className={`group relative flex h-[30px] min-w-0 max-w-52 shrink-0 cursor-default items-center gap-2 rounded-t-md pl-3 pr-1.5 text-xs transition-fast ${
              selected ? "bg-base text-ink shadow-[inset_0_1px_0_var(--color-hairline),inset_1px_0_0_var(--color-hairline),inset_-1px_0_0_var(--color-hairline)]" : "text-ink-muted hover:bg-raised hover:text-ink"
            } ${dropAt === -1 - i ? "bg-drop text-ink" : ""}`}
          >
            {dropAt === i && <span className="absolute -left-0.5 top-1.5 h-5 w-0.5 rounded-full bg-accent-400" />}
            {dropAt === i + 1 && i === tabs.length - 1 && <span className="absolute -right-0.5 top-1.5 h-5 w-0.5 rounded-full bg-accent-400" />}
            <FileIcon kind="folder" size={14} />
            <span className="truncate">{label}</span>
            <button
              aria-label={`Close ${label}`}
              className={`no-drag grid size-5 shrink-0 place-items-center rounded text-ink-faint transition-fast hover:bg-overlay hover:text-ink ${
                selected ? "opacity-100" : "opacity-0 group-hover:opacity-100"
              }`}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={() => updateWorkspace((ws) => W.closeTab(ws, pane, i))}
            >
              <I.Close size={12} />
            </button>
          </div>
        );
      })}
      <button
        aria-label="New tab"
        title={newTabTitle}
        className="btn-icon mb-0.5 ml-0.5 size-7 shrink-0"
        onClick={() => updateWorkspace((ws) => W.openTab(ws, pane, W.activeTab(ws, pane)?.path ?? "C:\\"))}
      >
        <I.Plus size={14} />
      </button>
    </div>
  );
}

/* ───────────────────────────── Toolbar ───────────────────────────── */

function Toolbar({ pane, tab, rt }: { pane: PaneId; tab: W.Tab; rt: TabRuntime }) {
  const backTitle = useTitle("Back", "nav.back");
  const fwdTitle = useTitle("Forward", "nav.forward");
  const upTitle = useTitle("Up", "nav.up");
  const refreshTitle = useTitle("Refresh", "nav.refresh");
  return (
    <div className="flex h-11 shrink-0 items-center gap-1 border-b border-hairline px-2">
      <button className="btn-icon" title={backTitle} aria-label="Back" disabled={!W.canGoBack(tab)} onClick={() => updateWorkspace((ws) => W.back(ws, pane))}>
        <I.Back />
      </button>
      <button className="btn-icon" title={fwdTitle} aria-label="Forward" disabled={!W.canGoForward(tab)} onClick={() => updateWorkspace((ws) => W.forward(ws, pane))}>
        <I.Forward />
      </button>
      <button className="btn-icon" title={upTitle} aria-label="Up" disabled={!/[\\/]./.test(tab.path)} onClick={() => goUp(pane)}>
        <I.Up />
      </button>
      <button className="btn-icon" title={refreshTitle} aria-label="Refresh" onClick={() => void rt.refresh()}>
        <I.Refresh size={15} />
      </button>
      <Breadcrumb pane={pane} path={tab.path} />
      <SearchBox pane={pane} rt={rt} tab={tab} />
      <ViewControls pane={pane} tab={tab} />
    </div>
  );
}

function Breadcrumb({ pane, path }: { pane: PaneId; path: string }) {
  const editing = useApp((s) => s.editingPath === pane);
  const [dropOn, setDropOn] = useState<string | null>(null);
  const crumbs = breadcrumbs(path).map((c, i) => (i === 0 && /^[A-Za-z]:$/.test(c.path) ? { ...c, path: `${c.path}\\` } : c));
  // Long paths collapse their middle; the ends are what people navigate by.
  const shown = crumbs.length > 5 ? [crumbs[0]!, null, ...crumbs.slice(-3)] : crumbs;

  if (editing) return <PathEditor pane={pane} path={path} />;

  return (
    // Right-aligned with overflow clipped at the *start*: in a narrow pane the current
    // folder stays visible and the root is what disappears (under a fade).
    <div
      className="field ml-1 flex min-w-0 flex-1 cursor-text items-center justify-end gap-0.5 overflow-hidden pl-2 pr-1 [mask-image:linear-gradient(to_right,transparent,black_8px)]"
      onClick={(e) => e.target === e.currentTarget && setEditingPath(pane)}
      title="Click to type a path"
    >
      {shown.map((c, i) =>
        c === null ? (
          <span key="gap" className="flex items-center">
            <I.Chevron size={12} className="text-ink-faint" />
            <button className="rounded px-1 text-ink-faint hover:bg-raised hover:text-ink" onClick={() => navigate(crumbs[crumbs.length - 4]!.path, pane)}>
              …
            </button>
          </span>
        ) : (
          <span key={c.path} className="flex shrink-0 items-center">
            {i > 0 && <I.Chevron size={12} className="shrink-0 text-ink-faint" />}
            <button
              onClick={() => navigate(c.path, pane)}
              onDragOver={(e) => {
                const drag = activeDrag();
                if (!drag || !e.dataTransfer.types.includes(DRAG_MIME)) return;
                const op = dropOperation(drag.paths, c.path, { ctrl: e.ctrlKey, shift: e.shiftKey });
                if (dropRejection(drag.paths, c.path, op)) return;
                e.preventDefault();
                setDropOn(c.path);
              }}
              onDragLeave={() => setDropOn(null)}
              onDrop={(e) => {
                e.preventDefault();
                setDropOn(null);
                const p = decodePayload(e.dataTransfer.getData(DRAG_MIME));
                if (p) dropPaths(p.paths, c.path, { ctrl: e.ctrlKey, shift: e.shiftKey });
              }}
              className={`truncate rounded px-1.5 py-0.5 text-xs transition-fast hover:bg-raised hover:text-ink ${
                i === shown.length - 1 ? "font-medium text-ink" : "text-ink-muted"
              } ${dropOn === c.path ? "bg-drop text-ink" : ""}`}
            >
              {c.label}
            </button>
          </span>
        ),
      )}
      {/* Fills spare width (so short paths sit left) and shrinks first when they do not fit. */}
      <div className="h-full min-w-6 flex-1" onClick={() => setEditingPath(pane)} />
    </div>
  );
}

function PathEditor({ pane, path }: { pane: PaneId; path: string }) {
  const ref = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  useCommand("ui.cancel", () => setEditingPath(null));
  return (
    <form
      className="ml-1 min-w-0 flex-1"
      onSubmit={(e) => {
        e.preventDefault();
        const v = ref.current?.value.trim().replace(/^"|"$/g, "");
        if (v) navigate(v, pane);
        else setEditingPath(null);
      }}
    >
      <input
        ref={ref}
        defaultValue={path}
        spellCheck={false}
        aria-label="Path"
        className="field font-mono text-[12px]"
        onBlur={() => setEditingPath(null)}
      />
    </form>
  );
}

function SearchBox({ pane, rt, tab }: { pane: PaneId; rt: TabRuntime; tab: W.Tab }) {
  const snap = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  const focusReq = useApp((s) => s.focusSearch);
  const ref = useRef<HTMLInputElement>(null);
  const [focused, setFocused] = useState(false);

  useEffect(() => {
    if (focusReq?.pane === pane) {
      ref.current?.focus();
      ref.current?.select();
    }
  }, [focusReq, pane]);

  // Escape in the box clears it first, then hands focus back to the list.
  useCommand(
    "ui.cancel",
    () => {
      if (rt.getSnapshot().search.text) rt.setSearch({ text: "" });
      else (ref.current?.closest("section")?.querySelector(".list") as HTMLElement | null)?.focus();
    },
    focused,
  );

  const everywhere = snap.search.scope === "everywhere";
  const placeholder = everywhere ? "Search everywhere" : `Search ${basename(tab.path)}`;

  return (
    <label className={`field relative ml-1 flex shrink-0 items-center gap-2 transition-std ${focused ? "w-[clamp(180px,34%,320px)]" : "w-[clamp(140px,24%,240px)]"}`}>
      <I.Search size={14} className={snap.search.text ? "text-accent-300" : "text-ink-faint"} />
      <input
        ref={ref}
        value={snap.search.text}
        placeholder={placeholder}
        spellCheck={false}
        aria-label={placeholder}
        className="h-full min-w-0 flex-1 bg-transparent text-xs text-ink outline-none placeholder:text-ink-faint"
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onChange={(e) => rt.setSearch({ text: e.target.value })}
      />
      {snap.search.text ? (
        <button aria-label="Clear search" className="grid size-5 place-items-center rounded text-ink-faint hover:bg-raised hover:text-ink" onClick={() => rt.setSearch({ text: "" })}>
          <I.Close size={12} />
        </button>
      ) : (
        !focused && <CommandChord id="search.focus" className="opacity-80" />
      )}
    </label>
  );
}

const VIEWS: { mode: ViewMode; label: string; icon: React.ReactNode; cmd: string }[] = [
  { mode: "Details", label: "Details", icon: <I.Details />, cmd: "view.details" },
  { mode: "List", label: "List", icon: <I.List />, cmd: "view.list" },
  { mode: "Tiles", label: "Tiles", icon: <I.Tiles />, cmd: "view.tiles" },
  { mode: "Grid", label: "Grid", icon: <I.Grid />, cmd: "view.grid" },
];

const SORTS: { key: SortKey; label: string }[] = [
  { key: "Name", label: "Name" },
  { key: "Size", label: "Size" },
  { key: "Extension", label: "Type" },
  { key: "Modified", label: "Date modified" },
  { key: "Created", label: "Date created" },
];

function ViewControls({ pane, tab }: { pane: PaneId; tab: W.Tab }) {
  const sortMenu = usePopover();
  const setSort = (sort: Sort) => updateWorkspace((ws) => W.setSort(ws, pane, sort));
  const items: MenuEntry[] = [
    { heading: "Sort by" },
    ...SORTS.map((s) => ({ id: s.key, label: s.label, checked: tab.sort.key === s.key, run: () => setSort(tab.sort.key === s.key ? tab.sort : W.toggleSort(tab.sort, s.key)) })),
    "separator",
    { id: "asc", label: "Ascending", checked: !tab.sort.desc, run: () => setSort({ ...tab.sort, desc: false }) },
    { id: "desc", label: "Descending", checked: tab.sort.desc, run: () => setSort({ ...tab.sort, desc: true }) },
    "separator",
    { id: "dirs", label: "Folders first", checked: tab.sort.dirs_first, run: () => setSort({ ...tab.sort, dirs_first: !tab.sort.dirs_first }) },
  ];
  return (
    <div className="ml-1 flex shrink-0 items-center gap-0.5">
      <button className="btn-icon" aria-label="Sort" title="Sort" onClick={(e) => sortMenu.toggle(e.currentTarget)}>
        <I.Sort size={15} />
      </button>
      <Popover anchor={sortMenu.anchor} onClose={sortMenu.close} align="end">
        <Menu items={items} onClose={sortMenu.close} />
      </Popover>
      <div className="mx-1 h-4 w-px bg-hairline" />
      <div role="radiogroup" aria-label="View" className="flex rounded-md bg-surface p-0.5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        {VIEWS.map((v) => (
          <ViewButton key={v.mode} {...v} pressed={tab.view === v.mode} onClick={() => updateWorkspace((ws) => W.setView(ws, pane, v.mode))} />
        ))}
      </div>
    </div>
  );
}

function ViewButton({ label, icon, cmd, pressed, onClick }: { label: string; icon: React.ReactNode; cmd: string; pressed: boolean; onClick: () => void }) {
  const title = useTitle(label, cmd);
  return (
    <button role="radio" aria-checked={pressed} aria-pressed={pressed} aria-label={label} title={title} onClick={onClick} className="btn-icon size-6">
      {icon}
    </button>
  );
}

/* ───────────────────────────── Search filters ───────────────────────────── */

function SearchFilters({ rt }: { rt: TabRuntime }) {
  const snap = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  const index = useApp((s) => s.index);
  const enabling = useApp((s) => s.enabling);
  if (snap.mode !== "search" && !snap.search.text) return null;

  const p = snap.search;
  const set = (patch: Partial<SearchParams>) => rt.setSearch(patch, true);
  const sum = snap.searchSummary;
  // Degraded means the walkdir fallback answered: slower, and limited to what it walked.
  const degraded = sum?.degraded ?? index?.state !== "Ready";

  return (
    <div className="shrink-0 border-b border-hairline bg-surface/60">
      <div className="flex h-10 items-center gap-1.5 overflow-x-auto px-3 [scrollbar-width:none]">
        <Segmented
          value={p.scope}
          options={[
            ["here", "This folder"],
            ["everywhere", "Everywhere"],
          ]}
          onChange={(scope) => set({ scope })}
        />
        <div className="mx-1 h-4 w-px bg-hairline" />
        <ChipSelect label="Kind" value={p.kind} onChange={(kind: KindChip) => set({ kind })} options={[["any", "Any kind"], ["files", "Files"], ["folders", "Folders"]]} />
        <ChipSelect
          label="Type"
          value={p.type}
          onChange={(type: TypeChip) => set({ type })}
          options={[["any", "Any type"], ["images", "Images"], ["documents", "Documents"], ["video", "Video"], ["audio", "Audio"], ["archives", "Archives"], ["code", "Code"]]}
        />
        <ChipSelect
          label="Size"
          value={p.size}
          onChange={(size: SizeChip) => set({ size })}
          options={[["any", "Any size"], ["tiny", "Under 100 KB"], ["small", "100 KB – 10 MB"], ["large", "10 MB – 1 GB"], ["huge", "Over 1 GB"]]}
        />
        <ChipSelect
          label="Modified"
          value={p.date}
          onChange={(date: DateChip) => set({ date })}
          options={[["any", "Any time"], ["today", "Today"], ["week", "Past week"], ["month", "Past month"], ["year", "Past year"]]}
        />
        <ChipSelect
          label="Match"
          value={p.mode}
          onChange={(mode) => set({ mode })}
          options={[["Substring", "Contains"], ["Fuzzy", "Fuzzy"], ["Glob", "Wildcard"], ["Regex", "Regex"]]}
        />
        <div className="ml-auto shrink-0 pl-3 text-[11px] tabular-nums text-ink-faint">
          {snap.loading ? (
            "Searching…"
          ) : sum ? (
            <>
              <span className="text-ink-muted">{sum.total_matched.toLocaleString()}</span>
              {sum.truncated ? "+" : ""} result{sum.total_matched === 1 ? "" : "s"}
              {snap.count < sum.total_matched && ` · showing ${snap.count.toLocaleString()}`}
              {" · "}
              {formatMicros(sum.elapsed_micros)}
            </>
          ) : null}
        </div>
      </div>
      {degraded && (
        <div className="flex items-center gap-2 border-t border-hairline bg-[color-mix(in_oklab,var(--color-warn)_7%,transparent)] px-3 py-1.5 text-[11px] text-ink-muted">
          <I.Warn size={13} className="shrink-0 text-warn" />
          <span className="min-w-0 flex-1">
            <span className="font-medium text-ink">Results may be incomplete.</span> Instant search isn't enabled, so Fulcrum is walking folders instead of reading the
            drive's index — slower, and limited to folders it can reach.
          </span>
          <button className="btn btn-primary h-6 shrink-0 px-2.5 text-[11px]" disabled={enabling || index?.state === "Scanning"} onClick={() => void enableInstantSearch()}>
            <I.Bolt size={12} />
            {index?.state === "Scanning" ? "Indexing…" : enabling ? "Waiting for permission…" : "Enable instant search"}
          </button>
        </div>
      )}
    </div>
  );
}

function formatMicros(us: number): string {
  if (us < 1000) return `${us} µs`;
  if (us < 1_000_000) return `${(us / 1000).toFixed(us < 10_000 ? 1 : 0)} ms`;
  return `${(us / 1_000_000).toFixed(2)} s`;
}

function Segmented<T extends string>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div role="radiogroup" className="flex shrink-0 rounded-full p-0.5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      {options.map(([v, label]) => (
        <button
          key={v}
          role="radio"
          aria-checked={value === v}
          onClick={() => onChange(v)}
          className={`h-[22px] rounded-full px-2.5 text-[11px] transition-fast ${value === v ? "bg-select text-accent-100 shadow-[inset_0_0_0_1px_var(--color-accent-700)]" : "text-ink-muted hover:text-ink"}`}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

function ChipSelect<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: [T, string][]; onChange: (v: T) => void }) {
  const pop = usePopover();
  const isDefault = value === options[0]?.[0];
  const current = options.find(([v]) => v === value)?.[1] ?? label;
  return (
    <>
      <button className="chip shrink-0" aria-pressed={!isDefault} aria-haspopup="menu" onClick={(e) => pop.toggle(e.currentTarget)}>
        {isDefault ? label : current}
        <I.ChevronDown size={11} className="opacity-70" />
      </button>
      <Popover anchor={pop.anchor} onClose={pop.close}>
        <Menu items={options.map(([v, l]) => ({ id: v, label: l, checked: v === value, run: () => onChange(v) }))} onClose={pop.close} />
      </Popover>
    </>
  );
}

/* ───────────────────────────── Status ───────────────────────────── */

function StatusBar({ rt }: { rt: TabRuntime }) {
  const snap = useSyncExternalStore(rt.subscribe, rt.getSnapshot);
  const showHidden = useApp((s) => s.settings?.behavior.list.show_hidden ?? false);
  const sel = snap.selection;

  // Summing sizes is O(selection); cap it so Ctrl+A on a million rows stays instant.
  let selBytes: number | null = null;
  if (!sel.isEmpty && sel.size <= 20_000) {
    selBytes = 0;
    for (const i of sel.indices()) {
      const e = rt.entryAt(i);
      if (e && e.kind !== "Dir") selBytes += e.size;
    }
  }

  return (
    <footer className="flex h-7 shrink-0 items-center gap-3 border-t border-hairline bg-surface px-3 text-[11px] tabular-nums text-ink-faint">
      <span>
        <span className="text-ink-muted">{snap.count.toLocaleString()}</span> {snap.mode === "search" ? "results" : "items"}
      </span>
      {!sel.isEmpty && (
        <span>
          <span className="text-accent-300">{sel.size.toLocaleString()}</span> selected
          {selBytes !== null && selBytes > 0 && ` · ${formatSize(selBytes)}`}
        </span>
      )}
      {snap.mode === "list" && snap.listSummary && (
        <span title="Time from request to the last row">listed in {formatMicros(snap.listSummary.elapsed_micros)}</span>
      )}
      {snap.mode === "list" && snap.listSummary && snap.listSummary.inaccessible > 0 && (
        <span className="text-warn">{snap.listSummary.inaccessible} not shown (no access)</span>
      )}
      {snap.loading && snap.count > 0 && <span className="text-accent-300">streaming…</span>}
      <span className="ml-auto flex items-center gap-1.5">{showHidden && <><I.Eye size={12} /> hidden files shown</>}</span>
    </footer>
  );
}
