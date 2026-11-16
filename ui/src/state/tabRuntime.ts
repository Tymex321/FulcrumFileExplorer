/**
 * Live data behind one tab: its directory listing or search results, its selection, and
 * transient UI state such as an in-progress rename.
 *
 * **The rows are not React state.** A listing is a plain array that streamed batches are
 * pushed onto; React sees only a small immutable `TabSnapshot` whose `version` bumps (at
 * most once per frame) when rows change. Rendering reads rows by index for the handful
 * that are on screen. That is what lets a 1M-entry directory stream in and scroll without
 * React ever diffing or copying the whole set.
 */
import { getApi } from "../api";
import type {
  EntryKind,
  FileEntry,
  Filters,
  FxError,
  ListOptions,
  ListSummary,
  MatchMode,
  Query,
  QuerySummary,
  SearchHit,
  Sort,
} from "../api/types";
import { join } from "../lib/path";
import { fromIndices, Selection } from "../lib/selection";
import { EMPTY_TYPEAHEAD, feed, findMatch, type TypeaheadState } from "../lib/typeahead";
import { frameScheduler } from "./store";

export type SearchScope = "here" | "everywhere";
export type KindChip = "any" | "files" | "folders";
export type TypeChip = "any" | "images" | "documents" | "video" | "audio" | "archives" | "code";
export type SizeChip = "any" | "tiny" | "small" | "large" | "huge";
export type DateChip = "any" | "today" | "week" | "month" | "year";

export interface SearchParams {
  text: string;
  scope: SearchScope;
  mode: MatchMode;
  kind: KindChip;
  type: TypeChip;
  size: SizeChip;
  date: DateChip;
}

export const EMPTY_SEARCH: SearchParams = {
  text: "",
  scope: "here",
  mode: "Substring",
  kind: "any",
  type: "any",
  size: "any",
  date: "any",
};

export const TYPE_EXTENSIONS: Record<Exclude<TypeChip, "any">, string[]> = {
  images: ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "heic", "tif", "tiff", "avif"],
  documents: ["pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "csv", "ppt", "pptx"],
  video: ["mp4", "mkv", "mov", "avi", "webm", "wmv", "m4v"],
  audio: ["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus"],
  archives: ["zip", "7z", "rar", "tar", "gz", "xz", "bz2", "zst", "iso"],
  code: ["rs", "ts", "tsx", "js", "jsx", "py", "c", "h", "cpp", "cs", "go", "java", "json", "toml", "yaml", "yml"],
};

const MB = 1024 * 1024;
const SIZE_RANGES: Record<Exclude<SizeChip, "any">, { min: number | null; max: number | null }> = {
  tiny: { min: null, max: 100 * 1024 },
  small: { min: 100 * 1024, max: 10 * MB },
  large: { min: 10 * MB, max: 1024 * MB },
  huge: { min: 1024 * MB, max: null },
};
const DAY = 86_400_000;
const DATE_SPANS: Record<Exclude<DateChip, "any">, number> = { today: DAY, week: 7 * DAY, month: 30 * DAY, year: 365 * DAY };

/** Translates the chip state into the Rust `Filters` — the UI never filters rows itself. */
export function filtersFor(p: SearchParams, showHidden: boolean, now = Date.now()): Filters {
  const kinds: EntryKind[] = p.kind === "files" ? ["File"] : p.kind === "folders" ? ["Dir"] : [];
  return {
    kinds,
    extensions: p.type === "any" ? [] : TYPE_EXTENSIONS[p.type],
    size: p.size === "any" ? null : SIZE_RANGES[p.size],
    modified: p.date === "any" ? null : { from: now - DATE_SPANS[p.date], to: null },
    created: null,
    include_hidden: showHidden,
    include_system: false,
  };
}

/** Builds the full query for a search page. */
export function queryFor(p: SearchParams, dir: string, sort: Sort, showHidden: boolean, offset: number, limit: number): Query {
  return {
    text: p.text,
    mode: p.mode,
    case: "Smart",
    scope: p.scope === "here" ? { Subtree: dir } : "All",
    filters: filtersFor(p, showHidden),
    // Name sort is useless for a search; relevance is what people expect by default.
    sort: sort.key === "Name" && p.mode === "Fuzzy" ? { ...sort, key: "Relevance" } : sort,
    limit,
    offset,
    match_full_path: false,
  };
}

export const SEARCH_PAGE = 2_000;

export interface TabSnapshot {
  /** Bumps whenever rows change. */
  version: number;
  mode: "list" | "search";
  count: number;
  loading: boolean;
  error: FxError | null;
  listSummary: ListSummary | null;
  search: SearchParams;
  searchSummary: QuerySummary | null;
  /** True while another search page is being fetched on scroll. */
  loadingMore: boolean;
  selection: Selection;
  /** Row being renamed inline. */
  renaming: number | null;
  /** Bumped to ask the view to scroll a row into view. */
  reveal: { index: number; seq: number } | null;
}

export interface LoadRequest {
  path: string;
  options: ListOptions;
}

function isFxError(e: unknown): e is FxError {
  return typeof e === "object" && e !== null && "kind" in e;
}

export function toFxError(e: unknown): FxError {
  if (isFxError(e)) return e;
  return { kind: "Other", detail: e instanceof Error ? e.message : String(e) };
}

export class TabRuntime {
  private entries: FileEntry[] = [];
  private hits: SearchHit[] = [];
  private snap: TabSnapshot = {
    version: 0,
    mode: "list",
    count: 0,
    loading: false,
    error: null,
    listSummary: null,
    search: EMPTY_SEARCH,
    searchSummary: null,
    loadingMore: false,
    selection: Selection.EMPTY,
    renaming: null,
    reveal: null,
  };
  private readonly listeners = new Set<() => void>();
  /** Incremented per request so batches from a superseded request are dropped. */
  private listGen = 0;
  private searchGen = 0;
  private searchTimer: ReturnType<typeof setTimeout> | null = null;
  private typeahead: TypeaheadState = EMPTY_TYPEAHEAD;
  private revealSeq = 0;
  /** Names to select once the next listing lands (after rename, new folder, jump-to-file). */
  private pendingSelect: { names: string[]; focus?: string; rename?: boolean } | null = null;

  /** The directory and options most recently loaded, for refresh and search scope. */
  current: LoadRequest | null = null;
  showHidden = false;

  private readonly flush = frameScheduler(() => this.publish({}));

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): TabSnapshot => this.snap;

  private publish(patch: Partial<TabSnapshot>, rowsChanged = true) {
    const mode = patch.mode ?? this.snap.mode;
    this.snap = {
      ...this.snap,
      ...patch,
      mode,
      count: mode === "search" ? this.hits.length : this.entries.length,
      version: rowsChanged ? this.snap.version + 1 : this.snap.version,
    };
    this.listeners.forEach((l) => l());
  }

  /* ───────────────────────── Row access (by index, never by copy) ───────────────────────── */

  get count(): number {
    return this.snap.mode === "search" ? this.hits.length : this.entries.length;
  }

  entryAt(i: number): FileEntry | undefined {
    return this.snap.mode === "search" ? this.hits[i]?.entry : this.entries[i];
  }

  hitAt(i: number): SearchHit | undefined {
    return this.snap.mode === "search" ? this.hits[i] : undefined;
  }

  pathAt(i: number): string | undefined {
    if (this.snap.mode === "search") return this.hits[i]?.path;
    const e = this.entries[i];
    return e && this.current ? join(this.current.path, e.name) : undefined;
  }

  /** Paths of the selection, capped so Ctrl+A on a million rows cannot build a giant array by accident. */
  selectedPaths(limit = 100_000): string[] {
    const out: string[] = [];
    for (const i of this.snap.selection.indices()) {
      const p = this.pathAt(i);
      if (p) out.push(p);
      if (out.length >= limit) break;
    }
    return out;
  }

  selectedEntries(limit = 100_000): FileEntry[] {
    const out: FileEntry[] = [];
    for (const i of this.snap.selection.indices()) {
      const e = this.entryAt(i);
      if (e) out.push(e);
      if (out.length >= limit) break;
    }
    return out;
  }

  /** The single focused row's entry, for the preview pane. */
  focusedIndex(): number | null {
    return this.snap.selection.focus;
  }

  /* ───────────────────────── Listing ───────────────────────── */

  async load(req: LoadRequest, opts: { keepSelection?: boolean } = {}): Promise<void> {
    const gen = ++this.listGen;
    const sameDir = this.current?.path === req.path;
    if (opts.keepSelection && sameDir && !this.pendingSelect) this.rememberSelection();
    this.current = req;
    this.showHidden = req.options.show_hidden;

    // Navigating away from a search ends it, like Explorer.
    if (!sameDir && this.snap.mode === "search") this.clearSearch(false);

    const fresh: FileEntry[] = [];
    this.entries = fresh;
    this.typeahead = EMPTY_TYPEAHEAD;
    this.publish({
      loading: true,
      error: null,
      listSummary: null,
      renaming: null,
      selection: Selection.EMPTY,
      reveal: sameDir ? this.snap.reveal : { index: 0, seq: ++this.revealSeq },
    });

    try {
      const api = await getApi();
      const summary = await api.listDir(req.path, req.options, (batch) => {
        if (gen !== this.listGen) return;
        // Push in place: no copying of rows already received.
        for (const e of batch) fresh.push(e);
        this.flush();
      });
      if (gen !== this.listGen) return;
      this.publish({ loading: false, listSummary: summary });
      this.applyPendingSelect();
    } catch (e) {
      if (gen !== this.listGen) return;
      this.publish({ loading: false, error: toFxError(e) });
    }
  }

  refresh(): Promise<void> {
    if (!this.current) return Promise.resolve();
    if (this.snap.mode === "search") {
      this.runSearch(0);
      return Promise.resolve();
    }
    return this.load(this.current, { keepSelection: true });
  }

  private rememberSelection() {
    const sel = this.snap.selection;
    if (sel.isEmpty || sel.size > 50_000) return;
    const names: string[] = [];
    for (const i of sel.indices()) {
      const e = this.entries[i];
      if (e) names.push(e.name);
    }
    const f = sel.focus !== null ? this.entries[sel.focus]?.name : undefined;
    this.pendingSelect = { names, focus: f };
  }

  /** Select these names after the next load (or now, if already present). */
  selectByName(names: string[], opts: { rename?: boolean } = {}): void {
    this.pendingSelect = { names, focus: names[0], rename: opts.rename };
    if (!this.snap.loading) this.applyPendingSelect();
  }

  private applyPendingSelect() {
    const p = this.pendingSelect;
    if (!p || this.snap.mode !== "list") return;
    const wanted = new Set(p.names.map((n) => n.toLowerCase()));
    const found: number[] = [];
    let focus: number | null = null;
    // One linear pass with a Set: fine for a million rows, and only after a load.
    for (let i = 0; i < this.entries.length && found.length < wanted.size; i++) {
      const name = this.entries[i]!.name.toLowerCase();
      if (wanted.has(name)) {
        found.push(i);
        if (p.focus && name === p.focus.toLowerCase()) focus = i;
      }
    }
    if (found.length === 0) return; // keep it pending: the entry may arrive on a later refresh
    this.pendingSelect = null;
    const f = focus ?? found[0]!;
    this.publish(
      {
        selection: fromIndices(found, f),
        renaming: p.rename ? f : null,
        reveal: { index: f, seq: ++this.revealSeq },
      },
      false,
    );
  }

  /* ───────────────────────── Search ───────────────────────── */

  setSearch(patch: Partial<SearchParams>, immediate = false): void {
    const search = { ...this.snap.search, ...patch };
    if (!search.text.trim()) {
      this.clearSearch(true, search);
      return;
    }
    this.publish({ search }, false);
    if (this.searchTimer) clearTimeout(this.searchTimer);
    // A short debounce coalesces fast typing without making the first result feel late.
    this.searchTimer = setTimeout(() => this.runSearch(0), immediate ? 0 : 70);
  }

  clearSearch(notify = true, keep?: SearchParams): void {
    if (this.searchTimer) clearTimeout(this.searchTimer);
    this.searchGen++;
    this.hits = [];
    const search = keep ? { ...keep, text: "" } : { ...this.snap.search, text: "" };
    if (notify) {
      this.publish({ mode: "list", search, searchSummary: null, loading: false, loadingMore: false, selection: Selection.EMPTY, renaming: null });
    } else {
      this.snap = { ...this.snap, mode: "list", search, searchSummary: null, loadingMore: false };
    }
  }

  private async runSearch(offset: number): Promise<void> {
    if (!this.current) return;
    const gen = offset === 0 ? ++this.searchGen : this.searchGen;
    const query = queryFor(this.snap.search, this.current.path, this.current.options.sort, this.showHidden, offset, SEARCH_PAGE);
    const target: SearchHit[] = offset === 0 ? [] : this.hits;
    if (offset === 0) {
      this.hits = target;
      this.publish({ mode: "search", loading: true, error: null, selection: Selection.EMPTY, renaming: null, reveal: { index: 0, seq: ++this.revealSeq } });
    } else {
      this.publish({ loadingMore: true }, false);
    }
    try {
      const api = await getApi();
      const summary = await api.search(query, (batch) => {
        if (gen !== this.searchGen) return;
        for (const h of batch) target.push(h);
        this.flush();
      });
      if (gen !== this.searchGen) return;
      this.publish({ loading: false, loadingMore: false, searchSummary: summary });
    } catch (e) {
      if (gen !== this.searchGen) return;
      this.publish({ loading: false, loadingMore: false, error: toFxError(e) });
    }
  }

  /** Called by the view when the user scrolls near the end of a partial result set. */
  loadMore(): void {
    const s = this.snap;
    if (s.mode !== "search" || s.loading || s.loadingMore || !s.searchSummary) return;
    if (this.hits.length >= s.searchSummary.total_matched) return;
    void this.runSearch(this.hits.length);
  }

  /* ───────────────────────── Selection & cursor ───────────────────────── */

  setSelection(selection: Selection, reveal = false): void {
    this.publish(
      { selection, reveal: reveal && selection.focus !== null ? { index: selection.focus, seq: ++this.revealSeq } : this.snap.reveal },
      false,
    );
  }

  startRename(index?: number): void {
    const i = index ?? this.snap.selection.focus ?? this.snap.selection.first();
    if (i === null || !this.entryAt(i)) return;
    this.publish({ renaming: i, selection: this.snap.selection.select(i), reveal: { index: i, seq: ++this.revealSeq } }, false);
  }

  endRename(): void {
    if (this.snap.renaming !== null) this.publish({ renaming: null }, false);
  }

  /** Type-to-select. Returns whether a row matched. */
  typeAhead(ch: string): boolean {
    this.typeahead = feed(this.typeahead, ch, performance.now());
    const hit = findMatch(this.typeahead.buffer, this.count, (i) => this.entryAt(i)?.name ?? "", this.snap.selection.focus);
    if (hit === null) return false;
    this.setSelection(this.snap.selection.select(hit), true);
    return true;
  }

  dispose(): void {
    this.listGen++;
    this.searchGen++;
    if (this.searchTimer) clearTimeout(this.searchTimer);
    this.listeners.clear();
  }
}

/* ───────────────────────── Registry: one runtime per open tab ───────────────────────── */

const runtimes = new Map<string, TabRuntime>();

export function runtimeFor(tabId: string): TabRuntime {
  let r = runtimes.get(tabId);
  if (!r) {
    r = new TabRuntime();
    runtimes.set(tabId, r);
  }
  return r;
}

/** Drops runtimes for tabs that no longer exist, so their row arrays can be collected. */
export function pruneRuntimes(liveIds: Set<string>): void {
  for (const [id, r] of runtimes) {
    if (!liveIds.has(id)) {
      r.dispose();
      runtimes.delete(id);
    }
  }
}

export function allRuntimes(): Iterable<TabRuntime> {
  return runtimes.values();
}
