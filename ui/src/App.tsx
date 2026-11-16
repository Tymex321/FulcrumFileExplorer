/**
 * Application shell: layout, the single key listener, and every app-level command
 * handler. Commands that act on a particular list (cursor movement, rename, select-all)
 * are registered by that list's `FileView`; everything else is here, in one table, so it
 * is obvious what each command does no matter where it is invoked from.
 */
import { useEffect, useRef } from "react";
import * as W from "./lib/workspace";
import {
  activePane,
  app,
  copyPathToClipboard,
  copySelection,
  deleteSelection,
  checkForUpdate,
  enableInstantSearch,
  focusSearch,
  goUp,
  init,
  newFolder,
  otherPane,
  openPalette,
  openSettings,
  paste,
  pin,
  runtimeOf,
  setEditingPath,
  setSplitRatio,
  tabOf,
  togglePreview,
  toggleSidebar,
  transferToOther,
  updateSettings,
  updateWorkspace,
  useApp,
} from "./state/app";
import { installKeyListener, useCommands } from "./state/commands";
import { getApi } from "./api";
import { Sidebar, TitleBar } from "./components/Chrome";
import { JobToasts } from "./components/Jobs";
import { CommandPalette, ConfirmDialog, ContextMenu, Notices } from "./components/Overlays";
import { Pane } from "./components/Pane";
import { PreviewPane } from "./components/Preview";
import { SettingsDialog } from "./components/SettingsDialog";

export function App() {
  const ready = useApp((s) => s.ready);
  const layout = useApp((s) => s.workspace.layout);
  const sidebar = useApp((s) => s.sidebarOpen);
  const preview = useApp((s) => s.previewOpen);
  const ratio = useApp((s) => s.splitRatio);

  useEffect(() => {
    void init();
    return installKeyListener();
  }, []);

  useGlobalCommands();

  if (!ready) return <Splash />;

  const split = layout !== "Single";
  return (
    <div className="flex h-full flex-col bg-base text-ink">
      <TitleBar />
      <div className="flex min-h-0 flex-1">
        {sidebar && <Sidebar />}
        <main className={`flex min-h-0 min-w-0 flex-1 ${layout === "Horizontal" ? "flex-col" : "flex-row"}`}>
          <div className="flex min-h-0 min-w-0" style={{ flex: split ? `${ratio} 1 0` : "1 1 0" }}>
            <Pane pane={0} />
          </div>
          {split && (
            <>
              <Splitter vertical={layout === "Vertical"} />
              <div className="flex min-h-0 min-w-0" style={{ flex: `${1 - ratio} 1 0` }}>
                <Pane pane={1} />
              </div>
            </>
          )}
        </main>
        {preview && <PreviewPane />}
      </div>
      <CommandPalette />
      <ContextMenu />
      <SettingsDialog />
      <ConfirmDialog />
      <JobToasts />
      <Notices />
    </div>
  );
}

function Splash() {
  return (
    <div className="grid h-full place-items-center bg-base">
      <div className="size-6 animate-pulse rounded-md bg-gradient-to-br from-accent-300 to-accent-600" />
    </div>
  );
}

/** Drag handle between panes; double-click evens the split. */
function Splitter({ vertical }: { vertical: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={ref}
      role="separator"
      aria-orientation={vertical ? "vertical" : "horizontal"}
      className={`group relative z-10 shrink-0 bg-hairline ${vertical ? "w-px cursor-col-resize" : "h-px cursor-row-resize"}`}
      onDoubleClick={() => setSplitRatio(0.5)}
      onPointerDown={(e) => {
        e.preventDefault();
        const main = ref.current?.parentElement;
        if (!main) return;
        const rect = main.getBoundingClientRect();
        const move = (ev: PointerEvent) =>
          setSplitRatio(vertical ? (ev.clientX - rect.left) / rect.width : (ev.clientY - rect.top) / rect.height);
        const up = () => {
          window.removeEventListener("pointermove", move);
          window.removeEventListener("pointerup", up);
          document.body.style.cursor = "";
        };
        document.body.style.cursor = vertical ? "col-resize" : "row-resize";
        window.addEventListener("pointermove", move);
        window.addEventListener("pointerup", up);
      }}
    >
      {/* A wider invisible hit area, so the 1px line is easy to grab. */}
      <div className={`absolute transition-fast group-hover:bg-accent-500/60 ${vertical ? "-left-[3px] top-0 h-full w-[7px]" : "-top-[3px] left-0 h-[7px] w-full"}`} />
    </div>
  );
}

function useGlobalCommands() {
  const ws = (fn: (w: W.Workspace) => W.Workspace) => updateWorkspace(fn);
  const view = (v: W.Tab["view"]) => () => ws((w) => W.setView(w, w.activePane, v));

  useCommands({
    "palette.files": () => openPalette("files"),
    "palette.commands": () => openPalette("commands"),
    "app.settings": () => openSettings("general"),
    "app.keybindings": () => openSettings("keys"),
    "index.enable": () => void enableInstantSearch(),
    "update.check": () => void checkForUpdate(true),

    "search.focus": () => focusSearch(),
    "search.everywhere": () => focusSearch(activePane(), true),

    "nav.back": () => ws((w) => W.back(w, w.activePane)),
    "nav.forward": () => ws((w) => W.forward(w, w.activePane)),
    "nav.up": () => goUp(),
    "nav.refresh": () => void runtimeOf()?.refresh(),
    "nav.editPath": () => setEditingPath(activePane()),

    "tab.new": () => ws((w) => W.openTab(w, w.activePane, W.activeTab(w)?.path ?? "C:\\")),
    "tab.close": () => ws((w) => W.closeTab(w, w.activePane, w.panes[w.activePane].active)),
    "tab.reopen": () => ws(W.restoreClosed),
    "tab.next": () => ws((w) => W.cycleTab(w, 1)),
    "tab.prev": () => ws((w) => W.cycleTab(w, -1)),

    "pane.splitVertical": () => ws((w) => W.setLayout(w, w.layout === "Vertical" ? "Single" : "Vertical")),
    "pane.splitHorizontal": () => ws((w) => W.setLayout(w, w.layout === "Horizontal" ? "Single" : "Horizontal")),
    "pane.single": () => ws((w) => W.setLayout(w, "Single")),
    "pane.focusOther": () => {
      const other = otherPane();
      if (other === null) return false;
      ws((w) => W.focusPane(w, other));
      // Move keyboard focus with it, so arrows drive the pane that now looks active.
      requestAnimationFrame(() => (document.querySelector('[data-pane-active="true"] .list') as HTMLElement | null)?.focus());
    },
    "pane.copyToOther": () => transferToOther("Copy"),
    "pane.moveToOther": () => transferToOther("Move"),
    "pane.mirror": () => {
      const path = tabOf()?.path;
      ws((w) => {
        const split = W.isSplit(w) ? w : W.setLayout(w, "Vertical");
        const other: 0 | 1 = split.activePane === 0 ? 1 : 0;
        return path ? W.navigate(split, other, path) : split;
      });
    },

    "view.details": view("Details"),
    "view.list": view("List"),
    "view.tiles": view("Tiles"),
    "view.grid": view("Grid"),
    "view.preview": () => togglePreview(),
    "view.sidebar": () => toggleSidebar(),
    "view.hidden": () =>
      updateSettings((s) => ({ ...s, behavior: { ...s.behavior, list: { ...s.behavior.list, show_hidden: !s.behavior.list.show_hidden } } })),
    "view.density": () =>
      updateSettings((s) => {
        const order = ["Compact", "Normal", "Comfortable"] as const;
        const next = order[(order.indexOf(s.appearance.density) + 1) % order.length]!;
        return { ...s, appearance: { ...s.appearance, density: next } };
      }),

    "file.copy": () => copySelection("copy"),
    "file.cut": () => copySelection("cut"),
    "file.paste": () => paste(),
    "file.trash": () => deleteSelection(false),
    "file.delete": () => deleteSelection(true),
    "file.newFolder": () => void newFolder(),
    "file.copyPath": () => copyPathToClipboard(),
    "file.pin": () => {
      const rt = runtimeOf();
      const sel = rt?.selectedEntries(50) ?? [];
      const dirs = sel.filter((e) => e.kind === "Dir");
      if (dirs.length && rt) for (const p of rt.selectedPaths(50)) pin(p);
      else {
        const here = tabOf()?.path;
        if (here) pin(here);
      }
    },
    "file.reveal": () => {
      const p = runtimeOf()?.selectedPaths(1)[0] ?? tabOf()?.path;
      if (p) void getApi().then((api) => api.reveal(p));
    },
    "file.properties": () => {
      // The preview pane doubles as the properties view.
      if (!app.get().previewOpen) togglePreview();
    },
  });
}
