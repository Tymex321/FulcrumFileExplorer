/**
 * Window chrome: the custom titlebar and the sidebar.
 *
 * The window is frameless (`decorations: false`), so the titlebar is a `.drag-region`
 * and **every** interactive child carries `.no-drag` — otherwise clicking a button moves
 * the window instead.
 */
import { type DragEvent as RDragEvent, useState } from "react";
import { hasTauri } from "../api";
import type { VolumeInfo } from "../api/types";
import { DRAG_MIME, decodePayload } from "../lib/dnd";
import { formatSize } from "../lib/format";
import { basename, isWithin } from "../lib/path";
import * as W from "../lib/workspace";
import {
  dropPaths,
  dismissUpdate,
  enableInstantSearch,
  installUpdate,
  movePin,
  navigate,
  openPalette,
  openSettings,
  pin,
  togglePreview,
  unpin,
  updateWorkspace,
  useApp,
  type Places,
} from "../state/app";
import { activeDrag } from "../state/drag";
import { FileIcon, I, Logo } from "./Icons";
import { CommandChord, useTitle } from "./Kbd";
import { Menu, Popover } from "./Popover";

/* ───────────────────────────── Titlebar ───────────────────────────── */

async function windowAction(action: "minimize" | "toggleMaximize" | "close") {
  if (!hasTauri()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  await w[action]();
}

export function TitleBar() {
  const layout = useApp((s) => s.workspace.layout);
  const preview = useApp((s) => s.previewOpen);
  const splitV = useTitle("Split side by side", "pane.splitVertical");
  const splitH = useTitle("Split top and bottom", "pane.splitHorizontal");
  const single = useTitle("Single pane", "pane.single");
  const prev = useTitle("Preview pane", "view.preview");
  const settings = useTitle("Settings", "app.settings");
  const setLayout = (l: typeof layout) => updateWorkspace((ws) => W.setLayout(ws, l));

  return (
    <header className="drag-region relative flex h-10 shrink-0 items-center gap-2 border-b border-hairline bg-surface pl-3" onDoubleClick={() => void windowAction("toggleMaximize")}>
      <Logo />
      <span className="text-[12px] font-semibold tracking-wide text-ink">Fulcrum</span>
      {!hasTauri() && <span className="rounded-full bg-raised px-2 py-0.5 text-[10px] text-ink-faint shadow-[inset_0_0_0_1px_var(--color-hairline)]">mock backend</span>}

      {/* The palette launcher sits dead centre, where the eye lands first. */}
      <div className="pointer-events-none absolute inset-x-0 flex justify-center">
        <button
          className="no-drag pointer-events-auto flex h-7 w-[min(440px,40vw)] items-center gap-2 rounded-md bg-base px-3 text-xs text-ink-faint shadow-[inset_0_0_0_1px_var(--color-hairline)] transition-fast hover:text-ink-muted hover:shadow-[inset_0_0_0_1px_var(--color-hairline-strong)]"
          onClick={() => openPalette("files")}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          <I.Search size={13} />
          <span className="flex-1 text-left">Jump to any file…</span>
          <CommandChord id="palette.files" />
        </button>
      </div>

      <div className="ml-auto flex items-center gap-0.5" onDoubleClick={(e) => e.stopPropagation()}>
        <button className="no-drag btn-icon" aria-pressed={layout === "Single"} aria-label="Single pane" title={single} onClick={() => setLayout("Single")}>
          <I.Single />
        </button>
        <button className="no-drag btn-icon" aria-pressed={layout === "Vertical"} aria-label="Split side by side" title={splitV} onClick={() => setLayout("Vertical")}>
          <I.SplitV />
        </button>
        <button className="no-drag btn-icon" aria-pressed={layout === "Horizontal"} aria-label="Split top and bottom" title={splitH} onClick={() => setLayout("Horizontal")}>
          <I.SplitH />
        </button>
        <div className="mx-1 h-4 w-px bg-hairline" />
        <button className="no-drag btn-icon" aria-pressed={preview} aria-label="Preview pane" title={prev} onClick={togglePreview}>
          <I.Preview />
        </button>
        <button className="no-drag btn-icon" aria-label="Command palette" title="Commands" onClick={() => openPalette("commands")}>
          <I.Command size={15} />
        </button>
        <button className="no-drag btn-icon" aria-label="Settings" title={settings} onClick={() => openSettings("general")}>
          <I.Settings />
        </button>
        <div className="ml-2 flex h-10 items-stretch">
          <WinButton label="Minimize" onClick={() => void windowAction("minimize")}>
            <I.Minimize size={14} />
          </WinButton>
          <WinButton label="Maximize" onClick={() => void windowAction("toggleMaximize")}>
            <I.Maximize size={12} />
          </WinButton>
          <WinButton label="Close" danger onClick={() => void windowAction("close")}>
            <I.Close size={14} />
          </WinButton>
        </div>
      </div>
    </header>
  );
}

function WinButton({ label, danger, onClick, children }: { label: string; danger?: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      aria-label={label}
      title={label}
      onClick={onClick}
      className={`no-drag grid w-11 place-items-center text-ink-muted transition-fast ${danger ? "hover:bg-[#c42b1c] hover:text-white" : "hover:bg-raised hover:text-ink"}`}
    >
      {children}
    </button>
  );
}

/* ───────────────────────────── Sidebar ───────────────────────────── */

function quickAccess(p: Places) {
  return [
    { label: "Home", path: p.home, icon: <I.Home size={15} /> },
    { label: "Desktop", path: p.desktop, icon: <I.Desktop size={15} /> },
    { label: "Documents", path: p.documents, icon: <I.Doc size={15} /> },
    { label: "Downloads", path: p.downloads, icon: <I.Download size={15} /> },
    { label: "Pictures", path: p.pictures, icon: <I.Picture size={15} /> },
    { label: "Music", path: p.music, icon: <I.Music size={15} /> },
    { label: "Videos", path: p.videos, icon: <I.Video size={15} /> },
  ];
}

export function Sidebar() {
  const volumes = useApp((s) => s.volumes);
  const pinned = useApp((s) => s.settings?.pinned ?? []);
  const places = useApp((s) => s.places);
  const current = useApp((s) => W.activeTab(s.workspace)?.path ?? "");
  const [pinDrop, setPinDrop] = useState<number | null>(null);
  const [pinDrag, setPinDrag] = useState<number | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; path: string } | null>(null);

  // Folders dragged onto the pinned section get pinned; pins dragged within it reorder.
  const onPinOver = (e: RDragEvent, slot: number) => {
    if (pinDrag !== null || e.dataTransfer.types.includes(DRAG_MIME)) {
      e.preventDefault();
      e.dataTransfer.dropEffect = pinDrag !== null ? "move" : "link";
      setPinDrop(slot);
    }
  };
  const onPinDrop = (e: RDragEvent) => {
    e.preventDefault();
    const slot = pinDrop ?? pinned.length;
    setPinDrop(null);
    if (pinDrag !== null) {
      movePin(pinDrag, slot > pinDrag ? slot - 1 : slot);
      setPinDrag(null);
      return;
    }
    const payload = decodePayload(e.dataTransfer.getData(DRAG_MIME));
    // Only folders can be pinned; the drag carries paths, so check against the source.
    const drag = activeDrag() ?? payload;
    for (const p of drag?.paths ?? []) pin(p);
  };

  return (
    <nav aria-label="Places" className="flex w-60 shrink-0 flex-col overflow-y-auto border-r border-hairline bg-surface pb-2">
      <div className="section-label">Quick access</div>
      {quickAccess(places).map((q) => (
        <PlaceRow key={q.path} label={q.label} path={q.path} icon={q.icon} active={current === q.path} onContext={(x, y) => setMenu({ x, y, path: q.path })} />
      ))}

      <div className="section-label flex items-center justify-between">
        <span>Pinned</span>
        <I.Pin size={12} />
      </div>
      <div onDragOver={(e) => onPinOver(e, pinned.length)} onDrop={onPinDrop} onDragLeave={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && setPinDrop(null)} className="min-h-8">
        {pinned.map((p, i) => (
          <div
            key={p}
            draggable
            onDragStart={(e) => {
              setPinDrag(i);
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", p);
            }}
            onDragEnd={() => {
              setPinDrag(null);
              setPinDrop(null);
            }}
            onDragOver={(e) => {
              e.stopPropagation();
              const r = e.currentTarget.getBoundingClientRect();
              onPinOver(e, e.clientY < r.top + r.height / 2 ? i : i + 1);
            }}
            className="relative"
          >
            {pinDrop === i && <div className="absolute inset-x-3 -top-px h-0.5 rounded-full bg-accent-400" />}
            {pinDrop === pinned.length && i === pinned.length - 1 && <div className="absolute inset-x-3 -bottom-px h-0.5 rounded-full bg-accent-400" />}
            <PlaceRow label={basename(p)} path={p} icon={<FileIcon kind="folder" size={15} />} active={current === p} onContext={(x, y) => setMenu({ x, y, path: p })} />
          </div>
        ))}
        {pinned.length === 0 && (
          <div className={`mx-3 rounded-md border border-dashed px-3 py-2 text-[11px] transition-fast ${pinDrop !== null ? "border-accent-500 bg-drop text-ink" : "border-hairline text-ink-faint"}`}>
            Drag folders here to pin them
          </div>
        )}
      </div>

      <div className="section-label">Drives</div>
      {volumes.map((v) => (
        <DriveRow key={v.id} v={v} active={isWithin(current, v.mount)} />
      ))}

      <div className="mt-auto" />
      <UpdateCard />
      <IndexCard />

      <Popover anchor={menu ? { x: menu.x, y: menu.y } : null} onClose={() => setMenu(null)}>
        {menu && (
          <Menu
            onClose={() => setMenu(null)}
            items={[
              { id: "open", label: "Open", icon: <I.Forward size={14} />, run: () => navigate(menu.path) },
              { id: "tab", label: "Open in new tab", icon: <I.Plus size={14} />, run: () => updateWorkspace((ws) => W.openTab(ws, ws.activePane, menu.path)) },
              "separator",
              pinned.includes(menu.path)
                ? { id: "unpin", label: "Unpin", icon: <I.Pin size={14} />, run: () => unpin(menu.path) }
                : { id: "pin", label: "Pin to sidebar", icon: <I.Pin size={14} />, run: () => pin(menu.path) },
            ]}
          />
        )}
      </Popover>
    </nav>
  );
}

function useDropInto(path: string) {
  const [over, setOver] = useState(false);
  return {
    over,
    handlers: {
      onDragOver: (e: RDragEvent) => {
        if (!e.dataTransfer.types.includes(DRAG_MIME)) return;
        e.preventDefault();
        e.stopPropagation();
        setOver(true);
      },
      onDragLeave: () => setOver(false),
      onDrop: (e: RDragEvent) => {
        if (!e.dataTransfer.types.includes(DRAG_MIME)) return;
        e.preventDefault();
        e.stopPropagation();
        setOver(false);
        const p = decodePayload(e.dataTransfer.getData(DRAG_MIME));
        if (p) dropPaths(p.paths, path, { ctrl: e.ctrlKey, shift: e.shiftKey });
      },
    },
  };
}

function PlaceRow({ label, path, icon, active, onContext }: { label: string; path: string; icon: React.ReactNode; active: boolean; onContext: (x: number, y: number) => void }) {
  const drop = useDropInto(path);
  return (
    <button
      onClick={() => navigate(path)}
      onAuxClick={(e) => e.button === 1 && updateWorkspace((ws) => W.openTab(ws, ws.activePane, path, { activate: false }))}
      onContextMenu={(e) => {
        e.preventDefault();
        onContext(e.clientX, e.clientY);
      }}
      title={path}
      {...drop.handlers}
      className={`relative mx-1.5 flex h-7 w-[calc(100%-12px)] items-center gap-2.5 rounded-md px-2.5 text-left text-xs transition-fast ${
        active ? "bg-raised text-ink" : "text-ink-muted hover:bg-raised/60 hover:text-ink"
      } ${drop.over ? "bg-drop! text-ink shadow-[inset_0_0_0_1px_var(--color-accent-400)]" : ""}`}
    >
      {active && <span className="absolute -left-1.5 top-1.5 h-4 w-[3px] rounded-r-full bg-accent-400" />}
      <span className={active ? "text-accent-300" : "text-ink-faint"}>{icon}</span>
      <span className="truncate">{label}</span>
    </button>
  );
}

function DriveRow({ v, active }: { v: VolumeInfo; active: boolean }) {
  const drop = useDropInto(v.mount);
  const used = v.total_bytes > 0 ? (v.total_bytes - v.free_bytes) / v.total_bytes : 0;
  // A nearly full drive is the one thing in the sidebar worth a colour change.
  const tone = used > 0.9 ? "bg-danger" : used > 0.8 ? "bg-warn" : "bg-gradient-to-r from-accent-600 to-accent-400";
  return (
    <button
      onClick={() => navigate(v.mount)}
      {...drop.handlers}
      title={`${v.label} (${v.id}:) · ${v.filesystem}${v.indexable ? "" : " · not indexable"}`}
      className={`relative mx-1.5 block w-[calc(100%-12px)] rounded-md px-2.5 py-2 text-left transition-fast ${
        active ? "bg-raised text-ink" : "text-ink-muted hover:bg-raised/60 hover:text-ink"
      } ${drop.over ? "bg-drop! shadow-[inset_0_0_0_1px_var(--color-accent-400)]" : ""}`}
    >
      {active && <span className="absolute -left-1.5 top-2.5 h-4 w-[3px] rounded-r-full bg-accent-400" />}
      <div className="flex items-center gap-2.5">
        <span className={active ? "text-accent-300" : "text-ink-faint"}>{v.removable ? <I.Usb size={15} /> : <I.Drive size={15} />}</span>
        <span className="min-w-0 flex-1 truncate text-xs">{v.label}</span>
        <span className="text-[10px] font-medium text-ink-faint">{v.id}:</span>
      </div>
      {v.total_bytes > 0 && (
        <div className="mt-1.5 pl-[26px]">
          <div className="h-1 overflow-hidden rounded-full bg-base shadow-[inset_0_0_0_1px_var(--color-hairline)]">
            <div className={`h-full rounded-full ${tone}`} style={{ width: `${Math.max(2, used * 100)}%` }} />
          </div>
          <div className="mt-1 flex justify-between text-[10px] tabular-nums text-ink-faint">
            <span>{formatSize(v.free_bytes)} free</span>
            <span>{formatSize(v.total_bytes)}</span>
          </div>
        </div>
      )}
    </button>
  );
}

/** Offers a newer version. Installing is always the user's choice; "Later" hides it until next launch. */
function UpdateCard() {
  const update = useApp((s) => s.update);
  const busy = useApp((s) => s.updateBusy);
  const dismissed = useApp((s) => s.updateDismissed);
  if (!update || (dismissed && busy !== "installing")) return null;

  if (busy === "installing") {
    return (
      <div className="mx-3 mt-3 rounded-lg px-3 py-2.5 text-[11px] text-ink-muted shadow-[inset_0_0_0_1px_var(--color-hairline)]" role="status">
        <div className="flex items-center gap-2">
          <I.Download size={13} className="text-accent-300" />
          <span>Installing {update.version}… Fulcrum will restart.</span>
        </div>
        <div className="mt-2 h-1 overflow-hidden rounded-full bg-base">
          <div className="bar-indeterminate h-full w-full" />
        </div>
      </div>
    );
  }

  return (
    <div className="mx-3 mt-3 overflow-hidden rounded-lg bg-gradient-to-b from-[color-mix(in_oklab,var(--color-accent-700)_22%,transparent)] to-transparent p-3 shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-accent-500)_35%,transparent)]">
      <div className="flex items-center gap-2 text-xs font-medium text-ink">
        <I.Download size={14} className="text-accent-300" />
        Fulcrum {update.version} is available
      </div>
      <p className="mt-1.5 text-[11px] leading-relaxed text-ink-muted">
        You have {update.current_version}. Updating keeps your settings and restarts Fulcrum.
      </p>
      <div className="mt-2.5 flex gap-2">
        <button className="btn btn-primary h-7 flex-1 justify-center text-[11px]" onClick={() => void installUpdate()}>
          Update now
        </button>
        <button className="btn h-7 px-3 text-[11px]" onClick={dismissUpdate}>
          Later
        </button>
      </div>
    </div>
  );
}

/**
 * The "Enable instant search" affordance. Shown whenever the index is Degraded or Idle,
 * with an explanation of why it needs a one-time install — elevation without a reason
 * is how users learn to distrust UAC prompts.
 */
function IndexCard() {
  const index = useApp((s) => s.index);
  const enabling = useApp((s) => s.enabling);
  if (!index) return null;

  if (index.state === "Ready") {
    return (
      <div className="mx-3 mt-3 flex items-center gap-2 rounded-lg px-3 py-2 text-[11px] text-ink-faint shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        <span className="size-1.5 rounded-full bg-ok shadow-[0_0_6px_var(--color-ok)]" />
        <span className="flex-1">
          <span className="text-ink-muted">{index.total_entries.toLocaleString()}</span> files indexed
        </span>
        <span title="Index memory">{formatSize(index.memory_bytes)}</span>
      </div>
    );
  }

  if (index.state === "Scanning") {
    return (
      <div className="mx-3 mt-3 rounded-lg px-3 py-2.5 text-[11px] text-ink-muted shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        <div className="flex items-center gap-2">
          <I.Bolt size={13} className="text-accent-300" />
          <span>Building index…</span>
        </div>
        <div className="mt-2 h-1 overflow-hidden rounded-full bg-base">
          <div className="bar-indeterminate h-full w-full" />
        </div>
      </div>
    );
  }

  return (
    <div className="mx-3 mt-3 overflow-hidden rounded-lg bg-gradient-to-b from-[color-mix(in_oklab,var(--color-accent-700)_22%,transparent)] to-transparent p-3 shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-accent-500)_35%,transparent)]">
      <div className="flex items-center gap-2 text-xs font-medium text-ink">
        <I.Bolt size={14} className="text-accent-300" />
        Instant search is off
      </div>
      <p className="mt-1.5 text-[11px] leading-relaxed text-ink-muted">
        Whole-drive search reads the NTFS index directly. That needs a small read-only service, installed once with administrator permission.
      </p>
      <button className="btn btn-primary mt-2.5 h-7 w-full justify-center text-[11px]" disabled={enabling} onClick={() => void enableInstantSearch()}>
        {enabling ? "Waiting for permission…" : "Enable instant search"}
      </button>
    </div>
  );
}

