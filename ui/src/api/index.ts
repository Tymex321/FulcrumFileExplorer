/**
 * The backend interface, and the switch between the real one and the mock.
 *
 * The UI is built against `FulcrumApi`, with a mock implementation, so the interface can
 * be developed and tested with no Rust backend running at all. `tauri.ts` implements it
 * on top of the real backend.
 */
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
  VolumeInfo,
} from "./types";

/** Receives streamed batches. Returning nothing; throw to abort. */
export type BatchHandler<T> = (batch: T[]) => void;

export interface FulcrumApi {
  listDir(
    path: string,
    options: ListOptions,
    onBatch: BatchHandler<FileEntry>,
  ): Promise<ListSummary>;

  search(query: Query, onBatch: BatchHandler<SearchHit>): Promise<QuerySummary>;

  stat(path: string): Promise<FileEntry>;
  volumes(): Promise<VolumeInfo[]>;

  indexStats(): Promise<IndexStats>;
  startScan(onProgress: (p: Progress) => void): Promise<void>;
  /** Triggers the one-time UAC prompt that installs the privileged index service. */
  enableInstantSearch(): Promise<void>;

  startOp(request: FsOpRequest, onProgress: (p: Progress) => void): Promise<OpId>;
  controlOp(op: OpId, control: OpControl): Promise<void>;

  createDir(path: string): Promise<void>;
  rename(path: string, newName: string): Promise<void>;
  openPath(path: string): Promise<void>;
  reveal(path: string): Promise<void>;

  icon(path: string, sizePx: number): Promise<Thumbnail>;
  thumbnail(path: string, sizePx: number): Promise<Thumbnail>;

  /**
   * First `maxBytes` of a file as UTF-8 text, for the preview pane. Optional because
   * the backend has no such command yet; the preview pane
   * degrades to metadata when a backend does not provide it.
   */
  readText?(path: string, maxBytes: number): Promise<string>;

  getSettings(): Promise<Settings>;
  setSettings(settings: Settings): Promise<void>;
  getSession(): Promise<SessionState>;
  setSession(session: SessionState): Promise<void>;
}

/**
 * True when running inside Tauri rather than a plain browser tab.
 *
 * `npm run dev` on its own serves the UI in a browser with no Rust behind it, which is
 * the normal way to work on the interface; `npm run tauri dev` runs the real thing.
 */
export function hasTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

let api: FulcrumApi | null = null;

/** Returns the real backend inside Tauri, and the mock everywhere else. */
export async function getApi(): Promise<FulcrumApi> {
  if (api) return api;
  if (hasTauri()) {
    const { tauriApi } = await import("./tauri");
    api = tauriApi;
  } else {
    const { mockApi } = await import("./mock");
    api = mockApi;
  }
  return api;
}

/** Forces a specific backend. For tests. */
export function setApi(next: FulcrumApi | null): void {
  api = next;
}
