/**
 * Application state and the actions that change it.
 *
 * Components read slices through `useApp(selector)`; keyboard handlers, menus and the
 * palette call the plain functions exported here. Keeping actions outside components
 * means a command does the same thing whether it came from a chord, a right-click or
 * the palette.
 */
import { getApi, hasTauri } from "../api";
import type {
  FsOpKind,
  FsOpRequest,
  IndexStats,
  ListOptions,
  OpControl,
  OpId,
  Progress,
  Settings,
  VolumeInfo,
} from "../api/types";
import { dropOperation, dropRejection } from "../lib/dnd";
import { basename, invalidNameReason, join, normalize, parent, uniqueName } from "../lib/path";
import * as W from "../lib/workspace";
import { keys } from "./commands";
import { createStore, useStore } from "./store";
import { allRuntimes, pruneRuntimes, runtimeFor, type TabRuntime, toFxError } from "./tabRuntime";

export type PaneId = 0 | 1;

export interface MenuState {
  x: number;
  y: number;
  pane: PaneId;
  /** Whether the right-click landed on a row (acts on the selection) or empty space. */
  onRow: boolean;
}

export interface ConfirmState {
  title: string;
  body: string;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
}

export interface Notice {
  id: number;
  text: string;
  tone: "info" | "error";
}

/** The user's home folder and the standard folders under "Quick access". */
export interface Places {
  home: string;
  desktop: string;
  documents: string;
  downloads: string;
  pictures: string;
  music: string;
  videos: string;
}

/** Home folder of the mock backend's sample file system. */
const MOCK_HOME = "C:\\Users\\alex";

function placesUnder(home: string): Places {
  return {
    home,
    desktop: join(home, "Desktop"),
    documents: join(home, "Documents"),
    downloads: join(home, "Downloads"),
    pictures: join(home, "Pictures"),
    music: join(home, "Music"),
    videos: join(home, "Videos"),
  };
}

/**
 * Asks the OS for the real known-folder locations, which also follows folders the user has
 * redirected (to OneDrive, say). Any folder the OS cannot report falls back to the usual
 * name under home. Outside Tauri this is the mock backend's home.
 */
async function resolvePlaces(): Promise<Places> {
  if (!hasTauri()) return placesUnder(MOCK_HOME);
  try {
    const path = await import("@tauri-apps/api/path");
    const home = await path.homeDir();
    const base = placesUnder(home);
    const [desktop, documents, downloads, pictures, music, videos] = await Promise.allSettled([
      path.desktopDir(),
      path.documentDir(),
      path.downloadDir(),
      path.pictureDir(),
      path.audioDir(),
      path.videoDir(),
    ]);
    const or = (r: PromiseSettledResult<string>, fallback: string) =>
      r.status === "fulfilled" && r.value ? r.value : fallback;
    return {
      home,
      desktop: or(desktop, base.desktop),
      documents: or(documents, base.documents),
      downloads: or(downloads, base.downloads),
      pictures: or(pictures, base.pictures),
      music: or(music, base.music),
      videos: or(videos, base.videos),
    };
  } catch {
    return placesUnder(MOCK_HOME);
  }
}

export interface AppState {
  ready: boolean;
  workspace: W.Workspace;
  settings: Settings | null;
  volumes: VolumeInfo[];
  index: IndexStats | null;
  /** The UAC prompt is up / the helper is being installed. */
  enabling: boolean;
  palette: null | "files" | "commands";
  settingsOpen: null | "general" | "keys";
  previewOpen: boolean;
  sidebarOpen: boolean;
  /** `keys` holds lowercased paths so each visible row checks "is this cut?" in O(1). */
  clipboard: { mode: "copy" | "cut"; paths: string[]; keys: Set<string> } | null;
  menu: MenuState | null;
  /** Pane whose breadcrumb is in raw-path edit mode. */
  editingPath: PaneId | null;
  /** Bumped to move keyboard focus into a pane's search box. */
  focusSearch: { pane: PaneId; seq: number } | null;
  confirm: ConfirmState | null;
  notices: Notice[];
  /** Fraction of the window the first pane takes in a split. */
  splitRatio: number;
  /** The user's home and known folders, shown under "Quick access". */
  places: Places;
}

export const app = createStore<AppState>({
  ready: false,
  workspace: W.initialWorkspace("C:\\"),
  settings: null,
  volumes: [],
  index: null,
  enabling: false,
  palette: null,
  settingsOpen: null,
  previewOpen: false,
  sidebarOpen: true,
  clipboard: null,
  menu: null,
  editingPath: null,
  focusSearch: null,
  confirm: null,
  notices: [],
  splitRatio: 0.5,
  places: placesUnder(MOCK_HOME),
});

export const useApp = <S>(selector: (s: AppState) => S) => useStore(app, selector);

const patch = (p: Partial<AppState>) => app.set((s) => ({ ...s, ...p }));

/* ───────────────────────────── Startup ───────────────────────────── */

export async function init(): Promise<void> {
  const api = await getApi();
  const [settings, session, volumes, index, places] = await Promise.all([
    api.getSettings(),
    api.getSession().catch(() => null),
    api.volumes().catch(() => [] as VolumeInfo[]),
    api.indexStats().catch(() => null),
    resolvePlaces(),
  ]);
  const fallback = volumes[0]?.mount ?? "C:\\";
  const workspace = settings.behavior.restore_session
    ? W.fromSession(session, fallback)
    : W.initialWorkspace(fallback, settings.behavior.default_view);
  applySettings(settings);
  patch({ ready: true, settings, volumes, index, workspace, places });
  watchIndex();
}

let indexTimer: ReturnType<typeof setTimeout> | null = null;

/** Polls index state while it is changing; idles once Ready. */
function watchIndex() {
  if (indexTimer) clearTimeout(indexTimer);
  const tick = async () => {
    const api = await getApi();
    const index = await api.indexStats().catch(() => null);
    patch({ index });
    const busy = index?.state === "Scanning";
    indexTimer = setTimeout(tick, busy ? 400 : 5000);
  };
  indexTimer = setTimeout(tick, 400);
}

/* ───────────────────────────── Settings ───────────────────────────── */

/** Reflects appearance settings onto <html>, where the CSS tokens pick them up. */
function applySettings(s: Settings) {
  const root = document.documentElement;
  root.dataset.accent = s.appearance.accent;
  root.dataset.density = s.appearance.density;
  root.dataset.motion = s.appearance.animations ? "on" : "off";
  root.style.fontSize = `${s.appearance.font_size}px`;
  document.body.style.fontSize = `${s.appearance.font_size}px`;
  keys.setOverrides(s.keybindings);
}

export function updateSettings(fn: (s: Settings) => Settings): void {
  const current = app.get().settings;
  if (!current) return;
  const next = fn(current);
  const listChanged =
    next.behavior.list.show_hidden !== current.behavior.list.show_hidden ||
    next.behavior.list.show_system !== current.behavior.list.show_system;
  applySettings(next);
  patch({ settings: next });
  void getApi().then((api) => api.setSettings(next));
  if (listChanged) refreshAll();
}

export function listOptionsFor(tab: W.Tab, s: Settings | null): ListOptions {
  const base = s?.behavior.list ?? {
    show_hidden: false,
    show_system: false,
    sort: W.DEFAULT_SORT,
    compute_dir_sizes: false,
  };
  return { ...base, sort: tab.sort };
}

/* ───────────────────────────── Workspace ───────────────────────────── */

let sessionTimer: ReturnType<typeof setTimeout> | null = null;

export function updateWorkspace(fn: (ws: W.Workspace) => W.Workspace): void {
  const prev = app.get().workspace;
  const next = fn(prev);
  if (next === prev) return;
  patch({ workspace: next });
  const live = new Set(next.panes.flatMap((p) => p.tabs.map((t) => t.id)));
  pruneRuntimes(live);
  // Session writes are debounced: navigating quickly should not hammer the disk.
  if (sessionTimer) clearTimeout(sessionTimer);
  sessionTimer = setTimeout(() => {
    void getApi().then((api) => api.setSession(W.toSession(app.get().workspace)));
  }, 400);
}

export const activePane = (): PaneId => app.get().workspace.activePane;
export const otherPane = (): PaneId | null => {
  const ws = app.get().workspace;
  return W.isSplit(ws) ? (ws.activePane === 0 ? 1 : 0) : null;
};

export function tabOf(pane: PaneId = activePane()): W.Tab | undefined {
  return W.activeTab(app.get().workspace, pane);
}

export function runtimeOf(pane: PaneId = activePane()): TabRuntime | null {
  const t = tabOf(pane);
  return t ? runtimeFor(t.id) : null;
}

export function navigate(path: string, pane: PaneId = activePane()): void {
  patch({ editingPath: null });
  updateWorkspace((ws) => W.focusPane(W.navigate(ws, pane, path), pane));
}

export function goUp(pane: PaneId = activePane()): void {
  const t = tabOf(pane);
  const up = t ? parent(t.path) : null;
  if (!t || !up) return;
  const from = basename(t.path);
  navigate(up, pane);
  // Land on the folder we came out of, as Explorer does.
  const rt = runtimeOf(pane);
  rt?.selectByName([from]);
}

/** Opens a path the user activated: folders navigate, files go to the OS. */
export async function openItem(path: string, isDir: boolean, pane: PaneId = activePane(), newTab = false) {
  if (isDir) {
    if (newTab) updateWorkspace((ws) => W.openTab(ws, pane, path));
    else navigate(path, pane);
    return;
  }
  const api = await getApi();
  await api.openPath(path).catch((e) => notify(`Could not open ${basename(path)}: ${toFxError(e).kind}`, "error"));
}

/** Goes to a file's folder and selects it (palette jump, "open file location"). */
export function revealInPane(path: string, pane: PaneId = activePane()): void {
  const dir = parent(path);
  if (!dir) return navigate(path, pane);
  navigate(dir, pane);
  runtimeOf(pane)?.selectByName([basename(path)]);
}

export function refreshAll(): void {
  for (const rt of allRuntimes()) void rt.refresh();
}

/** Refreshes every tab showing one of `dirs`, after an operation changed them. */
function refreshDirs(dirs: Iterable<string>): void {
  const set = new Set([...dirs].map((d) => normalize(d).toLowerCase()));
  for (const rt of allRuntimes()) {
    const p = rt.current?.path;
    if (p && set.has(normalize(p).toLowerCase())) void rt.refresh();
  }
}

/* ───────────────────────────── Notices & dialogs ───────────────────────────── */

let noticeSeq = 0;
export function notify(text: string, tone: Notice["tone"] = "info"): void {
  const id = ++noticeSeq;
  patch({ notices: [...app.get().notices, { id, text, tone }].slice(-4) });
  setTimeout(() => patch({ notices: app.get().notices.filter((n) => n.id !== id) }), tone === "error" ? 6000 : 3200);
}

export const openPalette = (palette: AppState["palette"]) => patch({ palette, menu: null });
export const openSettings = (settingsOpen: AppState["settingsOpen"]) => patch({ settingsOpen, palette: null });
export const openMenu = (menu: MenuState | null) => patch({ menu });
export const setEditingPath = (editingPath: PaneId | null) => patch({ editingPath });
export const askConfirm = (confirm: ConfirmState | null) => patch({ confirm });
export const togglePreview = () => patch({ previewOpen: !app.get().previewOpen });
export const toggleSidebar = () => patch({ sidebarOpen: !app.get().sidebarOpen });
export const setSplitRatio = (r: number) => patch({ splitRatio: Math.min(0.8, Math.max(0.2, r)) });

let searchSeq = 0;
export function focusSearch(pane: PaneId = activePane(), everywhere = false): void {
  const rt = runtimeOf(pane);
  if (everywhere) rt?.setSearch({ scope: "everywhere" });
  patch({ focusSearch: { pane, seq: ++searchSeq } });
}

/* ───────────────────────────── Index ───────────────────────────── */

export async function enableInstantSearch(): Promise<void> {
  patch({ enabling: true });
  try {
    const api = await getApi();
    await api.enableInstantSearch();
    notify("Instant search enabled. Building the index…");
  } catch (e) {
    const err = toFxError(e);
    // Cancelling the UAC prompt is a choice, not an error worth shouting about.
    if (err.kind !== "Cancelled") notify(`Could not enable instant search (${err.kind}).`, "error");
  } finally {
    patch({ enabling: false });
    watchIndex();
  }
}

/* ───────────────────────────── File operations ───────────────────────────── */

export interface Job {
  id: OpId;
  request: FsOpRequest;
  progress: Progress | null;
  startedAt: number;
  /** Set when the job reached a terminal state; the toast lingers briefly, then goes. */
  finishedAt: number | null;
}

export const jobs = createStore<Job[]>([]);
export const useJobs = <S>(selector: (j: Job[]) => S) => useStore(jobs, selector);

const TERMINAL = new Set(["Done", "Cancelled", "Failed"]);

export async function startOp(kind: FsOpKind, sources: string[], destination: string | null): Promise<void> {
  if (sources.length === 0) return;
  const request: FsOpRequest = {
    kind,
    sources,
    destination,
    // The conflict dialog is how the user decides; never silently overwrite.
    conflict: "Ask",
    verify: false,
  };
  const api = await getApi();
  let id: OpId | null = null;
  const early: Progress[] = [];
  const onProgress = (p: Progress) => {
    if (id === null) {
      early.push(p); // progress can arrive before startOp resolves
      return;
    }
    applyProgress(id, p);
  };
  try {
    id = await api.startOp(request, onProgress);
  } catch (e) {
    notify(`Could not start ${kind.toLowerCase()}: ${toFxError(e).kind}`, "error");
    return;
  }
  const opId = id;
  jobs.set((js) => [...js, { id: opId, request, progress: null, startedAt: Date.now(), finishedAt: null }]);
  early.forEach((p) => applyProgress(opId, p));
}

function applyProgress(id: OpId, p: Progress) {
  let finished: Job | null = null;
  jobs.set((js) =>
    js.map((j) => {
      if (j.id !== id) return j;
      const done = TERMINAL.has(p.state);
      const next = { ...j, progress: p, finishedAt: done ? (j.finishedAt ?? Date.now()) : null };
      if (done && j.finishedAt === null) finished = next;
      return next;
    }),
  );
  if (finished) onJobFinished(finished);
}

function onJobFinished(job: Job) {
  const dirs = new Set<string>();
  for (const s of job.request.sources) {
    const d = parent(s);
    if (d) dirs.add(d);
  }
  if (job.request.destination) dirs.add(job.request.destination);
  refreshDirs(dirs);
  if (job.request.kind === "Move" || job.request.kind === "Trash" || job.request.kind === "Delete") {
    const cb = app.get().clipboard;
    if (cb?.mode === "cut") patch({ clipboard: null });
  }
  const errors = job.progress?.errors.length ?? 0;
  if (errors) notify(`${errors} item${errors === 1 ? "" : "s"} could not be processed.`, "error");
  // Finished toasts linger long enough to read, then clear themselves.
  setTimeout(() => jobs.set((js) => js.filter((j) => j.id !== job.id)), 4000);
}

export async function controlOp(id: OpId, control: OpControl): Promise<void> {
  const api = await getApi();
  await api.controlOp(id, control).catch((e) => notify(`Operation control failed: ${toFxError(e).kind}`, "error"));
}

export function dismissJob(id: OpId): void {
  jobs.set((js) => js.filter((j) => j.id !== id));
}

/* ───────────────────────────── Clipboard & editing ───────────────────────────── */

export function copySelection(mode: "copy" | "cut", pane: PaneId = activePane()): void {
  const paths = runtimeOf(pane)?.selectedPaths() ?? [];
  if (!paths.length) return;
  patch({ clipboard: { mode, paths, keys: new Set(paths.map((p) => p.toLowerCase())) } });
  notify(`${mode === "cut" ? "Cut" : "Copied"} ${paths.length.toLocaleString()} item${paths.length === 1 ? "" : "s"}`);
}

export function paste(pane: PaneId = activePane()): void {
  const cb = app.get().clipboard;
  const dest = tabOf(pane)?.path;
  if (!cb || !dest) return;
  const kind = cb.mode === "cut" ? "Move" : "Copy";
  const why = dropRejection(cb.paths, dest, kind);
  if (why && kind === "Move") return notify(why, "error");
  void startOp(kind, cb.paths, dest);
}

/** Drag-and-drop entry point shared by rows, panes, tabs and the sidebar. */
export function dropPaths(paths: string[], destDir: string, mods: { ctrl: boolean; shift: boolean }): void {
  const op = dropOperation(paths, destDir, mods);
  const why = dropRejection(paths, destDir, op);
  if (why) return notify(why, "error");
  void startOp(op, paths, destDir);
}

export function transferToOther(kind: "Copy" | "Move"): void {
  const other = otherPane();
  if (other === null) return notify("Split the window to copy between panes.", "info");
  const dest = tabOf(other)?.path;
  const paths = runtimeOf()?.selectedPaths() ?? [];
  if (!dest || !paths.length) return;
  const why = dropRejection(paths, dest, kind);
  if (why) return notify(why, "error");
  void startOp(kind, paths, dest);
}

export function deleteSelection(permanent: boolean, pane: PaneId = activePane()): void {
  const paths = runtimeOf(pane)?.selectedPaths() ?? [];
  if (!paths.length) return;
  const s = app.get().settings;
  const hard = permanent || !!s?.behavior.skip_recycle_bin;
  const run = () => void startOp(hard ? "Delete" : "Trash", paths, null);
  const what = paths.length === 1 ? `"${basename(paths[0]!)}"` : `${paths.length.toLocaleString()} items`;
  if (hard || s?.behavior.confirm_delete) {
    askConfirm({
      title: hard ? "Delete permanently?" : "Move to Recycle Bin?",
      body: hard ? `${what} will be deleted. This cannot be undone.` : `${what} will be moved to the Recycle Bin.`,
      confirmLabel: hard ? "Delete" : "Move to Recycle Bin",
      danger: hard,
      onConfirm: run,
    });
  } else run();
}

export async function commitRename(rt: TabRuntime, index: number, newName: string): Promise<void> {
  const path = rt.pathAt(index);
  const entry = rt.entryAt(index);
  rt.endRename();
  if (!path || !entry || newName === entry.name) return;
  const why = invalidNameReason(newName);
  if (why) return notify(why, "error");
  try {
    const api = await getApi();
    await api.rename(path, newName);
    rt.selectByName([newName]);
    const dir = parent(path);
    if (dir) refreshDirs([dir]);
  } catch (e) {
    const err = toFxError(e);
    notify(err.kind === "AlreadyExists" ? `"${newName}" already exists.` : `Rename failed: ${err.kind}`, "error");
  }
}

export async function newFolder(pane: PaneId = activePane()): Promise<void> {
  const rt = runtimeOf(pane);
  const dir = tabOf(pane)?.path;
  if (!rt || !dir) return;
  if (rt.getSnapshot().mode === "search") rt.clearSearch();
  const taken = new Set<string>();
  for (let i = 0; i < rt.count; i++) taken.add(rt.entryAt(i)?.name ?? "");
  const name = uniqueName("New folder", taken);
  try {
    const api = await getApi();
    await api.createDir(join(dir, name));
    rt.selectByName([name], { rename: true });
    await rt.refresh();
  } catch (e) {
    notify(`Could not create folder: ${toFxError(e).kind}`, "error");
  }
}

export function copyPathToClipboard(pane: PaneId = activePane()): void {
  const paths = runtimeOf(pane)?.selectedPaths(1000) ?? [];
  const text = paths.length ? paths.join("\r\n") : (tabOf(pane)?.path ?? "");
  void navigator.clipboard?.writeText(text).then(
    () => notify(paths.length > 1 ? `Copied ${paths.length} paths` : "Path copied"),
    () => notify("Clipboard unavailable", "error"),
  );
}

/* ───────────────────────────── Pinned folders ───────────────────────────── */

export function pin(path: string): void {
  updateSettings((s) => (s.pinned.some((p) => normalize(p) === normalize(path)) ? s : { ...s, pinned: [...s.pinned, normalize(path)] }));
}
export function unpin(path: string): void {
  updateSettings((s) => ({ ...s, pinned: s.pinned.filter((p) => normalize(p) !== normalize(path)) }));
}
export function movePin(from: number, to: number): void {
  updateSettings((s) => {
    const pinned = s.pinned.slice();
    const [p] = pinned.splice(from, 1);
    if (p === undefined) return s;
    pinned.splice(Math.max(0, Math.min(to, pinned.length)), 0, p);
    return { ...s, pinned };
  });
}

/** True when a path is affected by a pending cut, so its row renders dimmed. */
export function isCut(path: string, cb: AppState["clipboard"]): boolean {
  return !!cb && cb.mode === "cut" && cb.keys.has(path.toLowerCase());
}
