/**
 * In-memory backend for developing the UI without Rust.
 *
 * This is what makes the interface buildable on any machine with no backend running.
 *
 * It stands in for the Rust side faithfully enough that the UI cannot cheat:
 *
 * - **Big directories on purpose.** A mock that returns twelve files proves nothing about a
 *   virtual list. Some paths hold 120k entries, and `D:\Stress\million files` holds a
 *   million — the bar the file table has to clear.
 * - **Sorting and filtering happen here**, as they will in Rust. The UI never sorts.
 * - **Search returns `match_ranges` as UTF-8 byte offsets** and fuzzy scores, including
 *   non-ASCII names, so the highlighter is exercised for real.
 * - **Operations are two-phase** (scan, then transfer) with `eta_secs: null` until an
 *   estimate is honest, real pause/resume/cancel, and a conflict whenever a destination
 *   name already exists.
 * - **The index starts Degraded** and goes Scanning → Ready after "Enable instant search",
 *   so the elevation affordance can be developed end to end.
 */
import { utf16RangeToUtf8 } from "../lib/highlight";
import { basename, isWithin, join, normalize, parent, uniqueName, volumeOf } from "../lib/path";
import type { BatchHandler, FulcrumApi } from "./index";
import type {
  Attributes,
  ConflictPolicy,
  FileEntry,
  IndexStats,
  ListSummary,
  OpControl,
  OpId,
  Progress,
  Query,
  QuerySummary,
  SearchHit,
  SessionState,
  Settings,
  Sort,
  Thumbnail,
  VolumeInfo,
} from "./types";

const BATCH = 500;

/* ───────────────────────────── Synthetic tree ───────────────────────────── */

const STEMS = [
  "Quarterly report", "invoice", "Screenshot", "meeting notes", "backup", "export", "draft",
  "budget", "IMG", "render", "archive", "config", "index", "README", "Résumé", "naïve-bayes",
  "日本語メモ", "presentation", "contract", "scan", "build-log", "dataset", "thumbnail",
];
const EXTS = [
  "pdf", "png", "jpg", "txt", "xlsx", "zip", "mp4", "json", "md", "docx", "rs", "ts", "mp3",
  "csv", "pptx", "exe", "log", "7z", "py", "svg",
];
const FOLDER_STEMS = [
  "Projects", "Archive", "Clients", "Assets", "Reports", "Builds", "Photos", "Exports",
  "Scratch", "Taxes", "Receipts", "Old", "Shared", "Templates", "Research",
];

/** Deterministic PRNG, so a given directory always looks the same across reloads. */
function seeded(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0x100000000;
  };
}

function hashPath(path: string): number {
  let h = 2166136261;
  const p = path.toLowerCase();
  for (let i = 0; i < p.length; i++) {
    h ^= p.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

let nextId = 1;
const NOW = Date.now();

function attrs(patch: Partial<Attributes> = {}): Attributes {
  return {
    hidden: false,
    system: false,
    readonly: false,
    archive: false,
    compressed: false,
    encrypted: false,
    sparse: false,
    reparse: false,
    ...patch,
  };
}

function dir(name: string, rand: () => number, a: Partial<Attributes> = {}): FileEntry {
  const created = NOW - Math.floor(rand() * 9e10);
  return {
    id: nextId++,
    parent: 0,
    name,
    ext: null,
    size: 0,
    modified: Math.min(NOW, created + Math.floor(rand() * 3e10)),
    created,
    kind: "Dir",
    attrs: attrs(a),
  };
}

function file(name: string, size: number, rand: () => number, a: Partial<Attributes> = {}): FileEntry {
  const dot = name.lastIndexOf(".");
  const created = NOW - Math.floor(rand() * 9e10);
  return {
    id: nextId++,
    parent: 0,
    name,
    ext: dot > 0 ? name.slice(dot + 1).toLowerCase() : null,
    size,
    modified: Math.min(NOW, created + Math.floor(rand() * 3e10)),
    created,
    kind: "File",
    attrs: attrs(a),
  };
}

/** Hand-written folders near the top, so the demo looks like a real machine. */
const FIXED: Record<string, (r: () => number) => FileEntry[]> = {
  "C:\\": (r) => [
    dir("PerfLogs", r),
    dir("Program Files", r, { readonly: true }),
    dir("Program Files (x86)", r, { readonly: true }),
    dir("Users", r),
    dir("Windows", r, { system: true }),
    dir("$Recycle.Bin", r, { hidden: true, system: true }),
    dir("System Volume Information", r, { hidden: true, system: true }),
    file("pagefile.sys", 17_179_869_184, r, { hidden: true, system: true }),
    file("hiberfil.sys", 6_871_947_673, r, { hidden: true, system: true }),
  ],
  "C:\\Users": (r) => [dir("alex", r), dir("Public", r), dir("Default", r, { hidden: true })],
  "C:\\Users\\alex": (r) => [
    dir("Desktop", r),
    dir("Documents", r),
    dir("Downloads", r),
    dir("Music", r),
    dir("Pictures", r),
    dir("Videos", r),
    dir("Projects", r),
    dir(".config", r, { hidden: true }),
    dir("AppData", r, { hidden: true }),
    file("NTUSER.DAT", 3_407_872, r, { hidden: true, system: true }),
  ],
  "C:\\Users\\alex\\Pictures": (r) => [
    dir("Camera Roll", r),
    dir("Screenshots", r),
    dir("Wallpapers", r),
    ...Array.from({ length: 60 }, (_, i) => file(`IMG_2026${String(i).padStart(4, "0")}.jpg`, 2e6 + Math.floor(r() * 6e6), r)),
  ],
  "D:\\": (r) => [dir("Backups", r), dir("Datasets", r), dir("Media", r), dir("Stress", r)],
  "D:\\Stress": (r) => [dir("million files", r), dir("120k files", r), dir("empty", r), dir("unicode", r)],
  "D:\\Stress\\empty": () => [],
  "D:\\Stress\\unicode": (r) => [
    file("Résumé – 2026.docx", 48_211, r),
    file("naïve café notes.md", 2_311, r),
    file("日本語のファイル.txt", 1_024, r),
    file("📁 emoji folder plan.pdf", 88_000, r),
    file("Ελληνικά.xlsx", 12_288, r),
    file("Ünïcödé ßtraße.png", 404_000, r),
  ],
  "E:\\": (r) => [dir("DCIM", r), dir("Installers", r), file("autorun.inf", 120, r, { hidden: true })],
};

/** Paths with a fixed entry count, for stress testing. */
const HUGE: Record<string, number> = {
  "D:\\Stress\\million files": 1_000_000,
  "D:\\Stress\\120k files": 120_000,
  "C:\\Users\\alex\\Pictures\\Camera Roll": 120_000,
};

function generated(path: string): FileEntry[] {
  const rand = seeded(hashPath(path));
  const huge = HUGE[path] ?? (hashPath(path) % 7 === 0 ? 120_000 : 0);
  const total = huge || Math.floor(rand() * 1800) + 20;
  const out: FileEntry[] = [];
  const dirs = Math.min(Math.floor(total * 0.08), huge ? 24 : 18);

  for (let i = 0; i < dirs; i++) {
    out.push(dir(`${FOLDER_STEMS[i % FOLDER_STEMS.length]}${i >= FOLDER_STEMS.length ? ` ${i}` : ""}`, rand));
  }
  for (let i = dirs; i < total; i++) {
    const ext = EXTS[Math.floor(rand() * EXTS.length)]!;
    const stem = STEMS[i % STEMS.length]!;
    // Log-ish size distribution: mostly small files, a few enormous ones.
    const size = Math.floor(Math.exp(rand() * 21));
    const hidden = rand() < 0.01;
    out.push(file(`${stem} ${i}.${ext}`, size, rand, { hidden, readonly: rand() < 0.02 }));
  }
  return out;
}

const tree = new Map<string, FileEntry[]>();

function entriesFor(path: string): FileEntry[] {
  const key = normalize(path);
  let e = tree.get(key);
  if (!e) {
    const fixed = FIXED[key];
    e = fixed ? fixed(seeded(hashPath(key))) : generated(key);
    tree.set(key, e);
  }
  return e;
}

/** Sorted views are cached per directory, invalidated whenever that directory changes. */
const sortedCache = new Map<string, FileEntry[]>();
function invalidate(dirPath: string) {
  const prefix = `${normalize(dirPath)}|`;
  for (const k of sortedCache.keys()) if (k.startsWith(prefix)) sortedCache.delete(k);
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function compareEntries(sort: Sort): (a: FileEntry, b: FileEntry) => number {
  const dir = sort.desc ? -1 : 1;
  const byKey = (a: FileEntry, b: FileEntry): number => {
    switch (sort.key) {
      case "Size":
        return a.size - b.size;
      case "Modified":
        return a.modified - b.modified;
      case "Created":
        return a.created - b.created;
      case "Kind":
      case "Extension":
        return collator.compare(a.ext ?? "", b.ext ?? "");
      default:
        return 0;
    }
  };
  return (a, b) => {
    if (sort.dirs_first) {
      const ad = a.kind === "Dir" ? 0 : 1;
      const bd = b.kind === "Dir" ? 0 : 1;
      if (ad !== bd) return ad - bd; // folders stay on top in both directions
    }
    const k = byKey(a, b);
    return (k !== 0 ? k : collator.compare(a.name, b.name)) * dir;
  };
}

/* ───────────────────────────── Streaming ───────────────────────────── */

/**
 * Yields to the event loop without setTimeout's 4ms clamp, so a million-row listing
 * streams in about as long as the real IPC would take rather than eight seconds.
 */
function yieldTask(): Promise<void> {
  return new Promise((resolve) => {
    if (typeof MessageChannel === "undefined") {
      setTimeout(resolve, 0);
      return;
    }
    const ch = new MessageChannel();
    ch.port1.onmessage = () => {
      ch.port1.close();
      resolve();
    };
    ch.port2.postMessage(null);
  });
}

async function stream<T>(items: T[], onBatch: BatchHandler<T>): Promise<void> {
  for (let i = 0; i < items.length; i += BATCH) {
    onBatch(items.slice(i, i + BATCH));
    if ((i / BATCH) % 8 === 7) await yieldTask();
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/* ───────────────────────────── Search ───────────────────────────── */

/** Folders the mock "volume" contains even before they are browsed, so search has a corpus. */
const SEED_DIRS = [
  "C:\\",
  "C:\\Users",
  "C:\\Users\\alex",
  "C:\\Users\\alex\\Documents",
  "C:\\Users\\alex\\Downloads",
  "C:\\Users\\alex\\Desktop",
  "C:\\Users\\alex\\Projects",
  "C:\\Users\\alex\\Pictures",
  "C:\\Users\\alex\\Music",
  "D:\\",
  "D:\\Datasets",
  "D:\\Media",
  "D:\\Backups",
  "D:\\Stress\\unicode",
];

interface Matcher {
  test(name: string): { score: number; ranges: [number, number][] } | null;
}

function matcherFor(q: Query): Matcher {
  const smartSensitive = q.case === "Smart" && /[A-Z]/.test(q.text);
  const sensitive = q.case === "Sensitive" || smartSensitive;
  const fold = (s: string) => (sensitive ? s : s.toLowerCase());
  const needle = fold(q.text);

  switch (q.mode) {
    case "Fuzzy":
      return {
        test(name) {
          // Greedy subsequence with bonuses for word starts and contiguity — enough to
          // produce realistic scores and multi-range highlights.
          const hay = fold(name);
          const ranges: [number, number][] = [];
          let score = 0;
          let h = 0;
          let prev = -2;
          for (const ch of needle) {
            if (ch === " ") continue;
            const at = hay.indexOf(ch, h);
            if (at < 0) return null;
            const boundary = at === 0 || /[\s_\-.]/.test(hay[at - 1]!);
            score += 1 + (at === prev + 1 ? 5 : 0) + (boundary ? 8 : 0);
            const last = ranges[ranges.length - 1];
            if (last && last[1] === at) last[1] = at + 1;
            else ranges.push([at, at + 1]);
            prev = at;
            h = at + 1;
          }
          return { score: score * 10 - name.length, ranges };
        },
      };
    case "Regex": {
      let re: RegExp;
      try {
        re = new RegExp(q.text, sensitive ? "" : "i");
      } catch {
        return { test: () => null };
      }
      return {
        test(name) {
          const m = re.exec(name);
          return m ? { score: 0, ranges: [[m.index, m.index + m[0].length]] } : null;
        },
      };
    }
    case "Glob": {
      const src = q.text.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
      const re = new RegExp(`^${src}$`, sensitive ? "" : "i");
      return { test: (name) => (re.test(name) ? { score: 0, ranges: [[0, name.length]] } : null) };
    }
    default:
      return {
        test(name) {
          const at = fold(name).indexOf(needle);
          return at < 0 ? null : { score: 0, ranges: [[at, at + needle.length]] };
        },
      };
  }
}

function passesFilters(e: FileEntry, q: Query): boolean {
  const f = q.filters;
  if (f.kinds.length && !f.kinds.includes(e.kind)) return false;
  if (f.extensions.length && !(e.ext && f.extensions.includes(e.ext))) return false;
  if (f.size) {
    if (e.kind === "Dir") return false;
    if (f.size.min !== null && e.size < f.size.min) return false;
    if (f.size.max !== null && e.size > f.size.max) return false;
  }
  if (f.modified) {
    if (f.modified.from !== null && e.modified < f.modified.from) return false;
    if (f.modified.to !== null && e.modified > f.modified.to) return false;
  }
  if (f.created) {
    if (f.created.from !== null && e.created < f.created.from) return false;
    if (f.created.to !== null && e.created > f.created.to) return false;
  }
  if (!f.include_hidden && e.attrs.hidden) return false;
  if (!f.include_system && e.attrs.system) return false;
  return true;
}

/* ───────────────────────────── Index state ───────────────────────────── */

let indexState: IndexStats = {
  state: "Degraded",
  total_entries: 0,
  total_dirs: 0,
  memory_bytes: 0,
  last_scan_millis: 0,
  volumes: [],
  helper_connected: false,
};

/* ───────────────────────────── Operations ───────────────────────────── */

interface Job {
  paused: boolean;
  cancelled: boolean;
  resolve: ((c: { policy: ConflictPolicy; apply_to_all: boolean }) => void) | null;
}
const jobs = new Map<OpId, Job>();
let nextOp = 1;

function lookup(path: string): FileEntry | undefined {
  const dirPath = parent(path);
  if (!dirPath) return undefined;
  const name = basename(path).toLowerCase();
  return entriesFor(dirPath).find((e) => e.name.toLowerCase() === name);
}

function removeEntry(path: string) {
  const dirPath = parent(path);
  if (!dirPath) return;
  const list = entriesFor(dirPath);
  const name = basename(path).toLowerCase();
  const i = list.findIndex((e) => e.name.toLowerCase() === name);
  if (i >= 0) list.splice(i, 1);
  invalidate(dirPath);
}

function sizeOf(e: FileEntry) {
  return e.kind === "Dir" ? 48_000_000 : e.size;
}

/* ───────────────────────────── The API ───────────────────────────── */

let settings: Settings | null = null;
let session: SessionState | null = null;

function fail(kind: string, detail: string): never {
  // Mirrors the serialised FxError shape that Tauri rejects with.
  throw { kind, detail };
}

export const mockApi: FulcrumApi = {
  async listDir(path, options, onBatch): Promise<ListSummary> {
    const started = performance.now();
    const key = normalize(path);
    if (!/^[A-Z]:\\/.test(key)) fail("NotFound", `${path} does not exist`);
    if (/^[F-Z]:\\/.test(key)) fail("NotFound", `${key} is not a mounted volume`);
    if (key.toLowerCase().includes("system volume information")) fail("AccessDenied", `Access to ${key} is denied`);
    // Large directories take a moment to "read", like a real cold cache.
    await sleep(entriesFor(key).length > 50_000 ? 40 : 4);

    const cacheKey = `${key}|${options.sort.key}|${options.sort.desc}|${options.sort.dirs_first}|${options.show_hidden}|${options.show_system}`;
    let entries = sortedCache.get(cacheKey);
    if (!entries) {
      entries = entriesFor(key).filter(
        (e) => (options.show_hidden || !e.attrs.hidden) && (options.show_system || !e.attrs.system),
      );
      entries.sort(compareEntries(options.sort));
      sortedCache.set(cacheKey, entries);
    }
    await stream(entries, onBatch);
    return {
      total: entries.length,
      elapsed_micros: Math.round((performance.now() - started) * 1000),
      inaccessible: key === "C:\\" && !options.show_system ? 0 : 0,
    };
  },

  async search(query, onBatch): Promise<QuerySummary> {
    const started = performance.now();
    const degraded = indexState.state !== "Ready";
    if (!query.text.trim()) return { returned: 0, total_matched: 0, elapsed_micros: 0, truncated: false, degraded };

    for (const d of SEED_DIRS) entriesFor(d);
    const matcher = matcherFor(query);
    const limit = query.limit || 1000;
    const scope = query.scope;
    const matches: SearchHit[] = [];

    for (const [dirPath, list] of tree) {
      if (scope !== "All") {
        if ("Volume" in scope && volumeOf(dirPath) !== scope.Volume) continue;
        if ("Subtree" in scope && !isWithin(dirPath, scope.Subtree)) continue;
      }
      // The million-row stress folder would dominate every search; the real index has
      // no such problem, so keep the mock responsive.
      if (list.length > 200_000) continue;
      for (const entry of list) {
        const m = matcher.test(entry.name);
        if (!m || !passesFilters(entry, query)) continue;
        const ranges = m.ranges.flatMap(([a, b]) => utf16RangeToUtf8(entry.name, a, b));
        matches.push({ entry, volume: volumeOf(dirPath) ?? "C", path: join(dirPath, entry.name), score: m.score, match_ranges: ranges });
      }
    }

    if (query.sort.key === "Relevance" || query.mode === "Fuzzy") {
      matches.sort((a, b) => b.score - a.score || collator.compare(a.entry.name, b.entry.name));
    } else if (query.sort.key === "Path") {
      const d = query.sort.desc ? -1 : 1;
      matches.sort((a, b) => collator.compare(a.path, b.path) * d);
    } else {
      const cmp = compareEntries(query.sort);
      matches.sort((a, b) => cmp(a.entry, b.entry));
    }

    // The fallback walker is slow; the index is not. Make the difference visible.
    await sleep(degraded ? 180 : 6);
    const page = matches.slice(query.offset, query.offset + limit);
    await stream(page, onBatch);
    return {
      returned: page.length,
      total_matched: matches.length,
      elapsed_micros: Math.round((performance.now() - started) * 1000),
      truncated: false,
      degraded,
    };
  },

  async stat(path): Promise<FileEntry> {
    const e = lookup(path);
    if (!e) fail("NotFound", `${path} does not exist`);
    return e;
  },

  async volumes(): Promise<VolumeInfo[]> {
    return [
      { id: "C", mount: "C:\\", label: "Windows", filesystem: "Ntfs", total_bytes: 1_000_204_886_016, free_bytes: 412_316_860_416, removable: false, indexable: true },
      { id: "D", mount: "D:\\", label: "Data", filesystem: "Ntfs", total_bytes: 2_000_398_934_016, free_bytes: 154_000_000_000, removable: false, indexable: true },
      { id: "E", mount: "E:\\", label: "USB Drive", filesystem: "ExFat", total_bytes: 64_000_000_000, free_bytes: 3_100_000_000, removable: true, indexable: false },
    ];
  },

  async indexStats(): Promise<IndexStats> {
    return { ...indexState };
  },

  async startScan(onProgress): Promise<void> {
    const total = 1_284_339;
    indexState = { ...indexState, state: "Scanning" };
    for (let i = 0; i <= 12; i++) {
      onProgress({
        op: 0,
        kind: "Copy",
        state: i === 12 ? "Done" : "Running",
        total_bytes: 0,
        done_bytes: 0,
        total_items: total,
        done_items: Math.round((total * i) / 12),
        current: i < 7 ? "Reading MFT on C:" : "Reading MFT on D:",
        bytes_per_sec: 0,
        eta_secs: null,
        errors: [],
        pending_conflict: null,
      });
      await sleep(180);
    }
    indexState = {
      state: "Ready",
      total_entries: total,
      total_dirs: 184_201,
      memory_bytes: 41_000_000,
      last_scan_millis: 2_160,
      volumes: ["C", "D"],
      helper_connected: true,
    };
  },

  async enableInstantSearch(): Promise<void> {
    // No UAC to show in the browser; pretend the user accepted and the service started.
    await sleep(500);
    indexState = { ...indexState, state: "Scanning", helper_connected: true };
    void mockApi.startScan(() => {});
  },

  async startOp(request, onProgress): Promise<OpId> {
    const op: OpId = nextOp++;
    const job: Job = { paused: false, cancelled: false, resolve: null };
    jobs.set(op, job);

    void (async () => {
      const base = (): Progress => ({
        op,
        kind: request.kind,
        state: "Scanning",
        total_bytes: 0,
        done_bytes: 0,
        total_items: 0,
        done_items: 0,
        current: "",
        bytes_per_sec: 0,
        eta_secs: null,
        errors: [],
        pending_conflict: null,
      });
      const p = base();

      // Phase 1: scan. Totals grow; no ETA, because one would be a guess.
      const items = request.sources.map((s) => ({ path: s, entry: lookup(s) }));
      for (const it of items) {
        p.total_items += it.entry?.kind === "Dir" ? 140 : 1;
        p.total_bytes += it.entry ? sizeOf(it.entry) : 0;
        p.current = it.path;
        onProgress({ ...p });
        await sleep(Math.min(120, 600 / items.length));
        if (job.cancelled) return onProgress({ ...p, state: "Cancelled" });
      }
      if (request.kind === "Delete" || request.kind === "Trash") p.total_bytes = 0;

      // Phase 2: transfer.
      p.state = "Running";
      const rate = request.kind === "Copy" ? 380_000_000 : request.kind === "Move" ? 900_000_000 : 0;
      const startedAt = performance.now();
      let applyAll: ConflictPolicy | null = request.conflict === "Ask" ? null : request.conflict;

      for (const it of items) {
        if (!it.entry) {
          p.errors.push({ path: it.path, message: "The system cannot find the file specified." });
          continue;
        }
        const dest = request.destination ? normalize(request.destination) : null;
        let targetName = it.entry.name;

        if (dest && (request.kind === "Copy" || request.kind === "Move")) {
          const existing = entriesFor(dest).find((e) => e.name.toLowerCase() === targetName.toLowerCase());
          if (existing) {
            let policy = applyAll;
            if (!policy) {
              job.paused = true;
              onProgress({
                ...p,
                state: "Paused",
                eta_secs: null,
                pending_conflict: {
                  source: it.path,
                  destination: join(dest, existing.name),
                  source_size: it.entry.size,
                  dest_size: existing.size,
                  source_modified: it.entry.modified,
                  dest_modified: existing.modified,
                },
              });
              const choice = await new Promise<{ policy: ConflictPolicy; apply_to_all: boolean }>((r) => (job.resolve = r));
              job.paused = false;
              if (job.cancelled) return onProgress({ ...p, state: "Cancelled" });
              policy = choice.policy;
              if (choice.apply_to_all) applyAll = choice.policy;
            }
            if (policy === "Skip" || (policy === "KeepNewer" && existing.modified >= it.entry.modified)) {
              p.done_items += 1;
              p.total_bytes -= sizeOf(it.entry);
              continue;
            }
            if (policy === "Rename") {
              targetName = uniqueName(targetName, new Set(entriesFor(dest).map((e) => e.name)));
            } else {
              removeEntry(join(dest, existing.name));
            }
          }
        }

        // Transfer this item in ticks so pause/cancel and ETA behave.
        const bytes = request.kind === "Delete" || request.kind === "Trash" ? 0 : sizeOf(it.entry);
        let moved = 0;
        p.current = it.path;
        do {
          while (job.paused && !job.cancelled) {
            onProgress({ ...p, state: "Paused", bytes_per_sec: 0, eta_secs: null });
            await sleep(150);
          }
          if (job.cancelled) return onProgress({ ...p, state: "Cancelled", bytes_per_sec: 0, eta_secs: null });
          const step = rate ? Math.min(bytes - moved, rate / 10) : bytes;
          moved += step;
          p.done_bytes += step;
          const elapsed = (performance.now() - startedAt) / 1000;
          p.bytes_per_sec = elapsed > 0.25 ? Math.round(p.done_bytes / elapsed) : 0;
          // Honest ETA: only once throughput has been measured for a moment.
          p.eta_secs = p.bytes_per_sec > 0 ? Math.max(0, Math.round((p.total_bytes - p.done_bytes) / p.bytes_per_sec)) : null;
          onProgress({ ...p });
          await sleep(rate ? 100 : 40);
        } while (moved < bytes);

        // Apply the change to the synthetic tree, so listings reflect it.
        if (dest && (request.kind === "Copy" || request.kind === "Move")) {
          entriesFor(dest).push({ ...it.entry, id: nextId++, name: targetName });
          invalidate(dest);
        }
        if (request.kind !== "Copy") removeEntry(it.path);
        p.done_items += it.entry.kind === "Dir" ? 140 : 1;
      }

      onProgress({ ...p, state: p.errors.length && p.errors.length === items.length ? "Failed" : "Done", done_items: p.total_items, done_bytes: p.total_bytes, eta_secs: 0 });
      jobs.delete(op);
    })();

    return op;
  },

  async controlOp(op: OpId, control: OpControl): Promise<void> {
    const job = jobs.get(op);
    if (!job) return;
    if (control === "Pause") job.paused = true;
    else if (control === "Resume") job.paused = false;
    else if (control === "Cancel") {
      job.cancelled = true;
      job.resolve?.({ policy: "Skip", apply_to_all: false });
    } else if (typeof control === "object" && "Resolve" in control) {
      const r = job.resolve;
      job.resolve = null;
      r?.(control.Resolve);
    }
  },

  async createDir(path): Promise<void> {
    const dirPath = parent(path);
    if (!dirPath) fail("InvalidPath", path);
    const name = basename(path);
    if (entriesFor(dirPath).some((e) => e.name.toLowerCase() === name.toLowerCase())) fail("AlreadyExists", path);
    const e = dir(name, Math.random);
    e.created = e.modified = Date.now();
    entriesFor(dirPath).push(e);
    tree.set(normalize(path), []);
    invalidate(dirPath);
  },

  async rename(path, newName): Promise<void> {
    const dirPath = parent(path);
    const e = lookup(path);
    if (!dirPath || !e) fail("NotFound", path);
    const clash = entriesFor(dirPath).some((x) => x !== e && x.name.toLowerCase() === newName.toLowerCase());
    if (clash) fail("AlreadyExists", `${newName} already exists`);
    // Rust replaces the entry; mutate a copy so the UI cannot rely on aliasing.
    const list = entriesFor(dirPath);
    const dot = newName.lastIndexOf(".");
    list[list.indexOf(e)] = { ...e, name: newName, ext: e.kind === "Dir" ? null : dot > 0 ? newName.slice(dot + 1).toLowerCase() : null, modified: Date.now() };
    const sub = tree.get(normalize(path));
    if (sub) {
      tree.delete(normalize(path));
      tree.set(join(dirPath, newName), sub);
    }
    invalidate(dirPath);
  },

  async openPath(): Promise<void> {},
  async reveal(): Promise<void> {},

  async icon(): Promise<Thumbnail> {
    return { data_base64: "", width: 16, height: 16, is_fallback: true };
  },

  async thumbnail(path, sizePx): Promise<Thumbnail> {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    const isImage = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"].includes(ext);
    const data = isImage ? syntheticImage(path, sizePx) : null;
    await sleep(30 + (hashPath(path) % 60)); // thumbnails are slow; the UI must not wait on them
    if (!data) return { data_base64: "", width: sizePx, height: sizePx, is_fallback: true };
    return { data_base64: data, width: sizePx, height: Math.round(sizePx * 0.66), is_fallback: false };
  },

  async readText(path, maxBytes): Promise<string> {
    await sleep(20);
    const name = basename(path);
    const rand = seeded(hashPath(path));
    if (/\.(rs|ts|py|json|toml|js)$/i.test(name)) {
      const lines = [
        `// ${name}`,
        "",
        "use std::collections::HashMap;",
        "",
        "/// Synthetic preview text from the mock backend.",
        "pub fn main() {",
        `    let budget = ${Math.floor(rand() * 9000)};`,
        "    let mut seen = HashMap::new();",
        "    for i in 0..budget {",
        "        *seen.entry(i % 7).or_insert(0) += 1;",
        "    }",
        '    println!("{seen:?}");',
        "}",
      ];
      return lines.join("\n").slice(0, maxBytes);
    }
    const para = [
      `# ${name.replace(/\.\w+$/, "")}`,
      "",
      "Fulcrum reads the NTFS master file table directly and replays the USN journal,",
      "so search answers in microseconds instead of walking the disk.",
      "",
      ...Array.from({ length: 24 }, (_, i) => `${i + 1}. ${STEMS[Math.floor(rand() * STEMS.length)]} — ${Math.floor(rand() * 1e6).toLocaleString()} bytes reviewed`),
    ];
    return para.join("\n").slice(0, maxBytes);
  },

  async getSettings(): Promise<Settings> {
    settings ??= loadLocal("fulcrum.mock.settings") ?? defaultSettings();
    return structuredClone(settings);
  },
  async setSettings(next): Promise<void> {
    settings = structuredClone(next);
    saveLocal("fulcrum.mock.settings", settings);
  },
  async getSession(): Promise<SessionState> {
    session ??= loadLocal("fulcrum.mock.session") ?? { layout: "Single", panes: [[]], active_pane: 0, active_tab: [0] };
    return structuredClone(session);
  },
  async setSession(next): Promise<void> {
    session = structuredClone(next);
    saveLocal("fulcrum.mock.session", session);
  },
};

/** Settings and session survive a browser reload in dev, as they would on disk. */
function loadLocal<T>(key: string): T | null {
  try {
    const raw = globalThis.localStorage?.getItem(key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}
function saveLocal(key: string, value: unknown) {
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify(value));
  } catch {
    /* private mode or quota: dev convenience only */
  }
}

/** Draws a deterministic gradient "photo" so the image preview and grid have pixels. */
function syntheticImage(path: string, size: number): string | null {
  if (typeof document === "undefined") return null;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = Math.round(size * 0.66);
  const ctx = canvas.getContext?.("2d");
  if (!ctx) return null;
  const r = seeded(hashPath(path));
  const hue = Math.floor(r() * 360);
  const g = ctx.createLinearGradient(0, 0, canvas.width, canvas.height);
  g.addColorStop(0, `hsl(${hue} 70% 55%)`);
  g.addColorStop(1, `hsl(${(hue + 60) % 360} 60% 25%)`);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  for (let i = 0; i < 5; i++) {
    ctx.fillStyle = `hsla(${(hue + i * 40) % 360} 80% 70% / 0.35)`;
    ctx.beginPath();
    ctx.arc(r() * canvas.width, r() * canvas.height, r() * size * 0.3 + 6, 0, Math.PI * 2);
    ctx.fill();
  }
  // A horizon line, so it reads as a landscape rather than noise.
  ctx.fillStyle = `hsla(${(hue + 200) % 360} 40% 12% / 0.7)`;
  ctx.fillRect(0, canvas.height * 0.7, canvas.width, canvas.height * 0.3);
  return canvas.toDataURL("image/png").split(",")[1] ?? null;
}

function defaultSettings(): Settings {
  return {
    appearance: {
      accent: "Purple",
      density: "Normal",
      acrylic: false,
      animations: true,
      font_size: 13,
    },
    index: {
      instant_search: true,
      volumes: [],
      exclusions: [],
      rescan_on_start: false,
    },
    behavior: {
      single_click_open: false,
      confirm_delete: true,
      skip_recycle_bin: false,
      restore_session: true,
      default_view: "Details",
      list: {
        show_hidden: false,
        show_system: false,
        sort: { key: "Name", desc: false, dirs_first: true },
        compute_dir_sizes: false,
      },
      sort: { key: "Name", desc: false, dirs_first: true },
    },
    pinned: ["C:\\Users\\alex\\Projects", "D:\\Stress"],
    keybindings: [],
  };
}

/** Test hook: resets mutable mock state between tests. */
export function __resetMock(): void {
  tree.clear();
  sortedCache.clear();
  jobs.clear();
  settings = null;
  session = null;
  indexState = { state: "Degraded", total_entries: 0, total_dirs: 0, memory_bytes: 0, last_scan_millis: 0, volumes: [], helper_connected: false };
}
