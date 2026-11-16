/**
 * The real backend: thin wrappers over the Tauri commands in `src-tauri/src/commands.rs`.
 *
 * Streaming commands take a `Channel`, which is how a listing of
 * 500k entries reaches the UI progressively instead of as one enormous response.
 *
 * Argument names must match the Rust parameter names exactly — Tauri matches by name, and
 * a typo fails at runtime rather than at compile time, so keep these in lockstep with the
 * `#[tauri::command]` signatures.
 */
import { invoke, Channel } from "@tauri-apps/api/core";
import type { BatchHandler, FulcrumApi } from "./index";
import type {
  FileEntry,
  FsOpRequest,
  IndexStats,
  ListOptions,
  ListSummary,
  OpControl,
  OpId,
  Progress,
  Query,
  QuerySummary,
  SearchHit,
  SessionState,
  Settings,
  Thumbnail,
  UpdateInfo,
  VolumeInfo,
} from "./types";

function batchChannel<T>(onBatch: BatchHandler<T>): Channel<T[]> {
  const channel = new Channel<T[]>();
  channel.onmessage = onBatch;
  return channel;
}

function progressChannel(onProgress: (p: Progress) => void): Channel<Progress> {
  const channel = new Channel<Progress>();
  channel.onmessage = onProgress;
  return channel;
}

export const tauriApi: FulcrumApi = {
  listDir: (path, options: ListOptions, onBatch) =>
    invoke<ListSummary>("list_dir", {
      path,
      options,
      onBatch: batchChannel<FileEntry>(onBatch),
    }),

  search: (query: Query, onBatch) =>
    invoke<QuerySummary>("search", {
      query,
      onBatch: batchChannel<SearchHit>(onBatch),
    }),

  stat: (path) => invoke<FileEntry>("stat", { path }),
  volumes: () => invoke<VolumeInfo[]>("volumes"),

  indexStats: () => invoke<IndexStats>("index_stats"),
  startScan: (onProgress) =>
    invoke<void>("start_scan", { onProgress: progressChannel(onProgress) }),
  enableInstantSearch: () => invoke<void>("enable_instant_search"),

  startOp: (request: FsOpRequest, onProgress) =>
    invoke<OpId>("start_op", { request, onProgress: progressChannel(onProgress) }),
  controlOp: (op: OpId, control: OpControl) => invoke<void>("control_op", { op, control }),

  createDir: (path) => invoke<void>("create_dir", { path }),
  rename: (path, newName) => invoke<void>("rename", { path, newName }),
  openPath: (path) => invoke<void>("open_path", { path }),
  reveal: (path) => invoke<void>("reveal", { path }),

  icon: (path, sizePx) => invoke<Thumbnail>("icon", { path, sizePx }),
  thumbnail: (path, sizePx) => invoke<Thumbnail>("thumbnail", { path, sizePx }),

  checkForUpdate: () => invoke<UpdateInfo | null>("check_for_update"),
  installUpdate: () => invoke<void>("install_update"),

  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  getSession: () => invoke<SessionState>("get_session"),
  setSession: (session: SessionState) => invoke<void>("set_session", { session }),
};

/**
 * Frameless-window controls, backed by the permissions in
 * `src-tauri/capabilities/default.json` (minimize, toggle-maximize, close, start-dragging).
 * Double-clicking the title bar should call `toggleMaximize`.
 */
export const windowControls = {
  minimize: async () => (await import("@tauri-apps/api/window")).getCurrentWindow().minimize(),
  toggleMaximize: async () =>
    (await import("@tauri-apps/api/window")).getCurrentWindow().toggleMaximize(),
  close: async () => (await import("@tauri-apps/api/window")).getCurrentWindow().close(),
  startDragging: async () =>
    (await import("@tauri-apps/api/window")).getCurrentWindow().startDragging(),
};
