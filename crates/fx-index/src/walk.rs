//! Directory-walking index backend.
//!
//! The portable fallback, and the only backend that runs on macOS — so this is where the
//! index's *logic* gets tested during development, while `crate::mft` (Windows) handles the
//! Windows-specific data acquisition. Queries go through the same [`crate::query::execute`]
//! as the MFT backend, so tests here cover the matching code that ships on Windows.
//!
//! Used when: developing on macOS, the volume is not NTFS, or the elevated helper has not
//! been installed yet. Always reports [`IndexState::Degraded`] once built, so the UI can
//! explain why search covers only some folders.
//!
//! ## Ids
//!
//! There is no filesystem id to borrow, so ids are synthetic, drawn from one counter shared
//! by every root. That makes an id unique across the whole backend (so
//! [`IndexBackend::resolve_path`] needs no root hint), and because the counter never
//! restarts, an id the UI still holds from before a rescan can never resolve to a different
//! file afterwards.
//!
//! ## Staying live
//!
//! `notify` delivers path-keyed events whose shape differs by platform: FSEvents on macOS
//! does not pair the two halves of a rename and can collapse a burst into "rescan this
//! directory". Rather than trust event kinds, the watcher **re-stats every touched path**
//! once the coalescing window closes, so the store converges on what is actually on disk
//! whatever the platform reported. Explicit rename pairs (which Windows and Linux deliver)
//! are applied as true renames so the entry keeps its id.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use fx_core::{
    Attributes, BatchSink, ChangeCallback, ChangeEvent, EntryId, EntryKind, FxError, IndexBackend,
    IndexSettings, IndexState, IndexStats, OpState, ProgressSink, Query, QuerySummary, Result,
    SearchHit, VolumeId, WatchHandle,
};
use parking_lot::{Mutex, RwLock};

use crate::query::{execute, store_on_volume};
use crate::store::{EntryStore, Meta, RecordIx, NO_RECORD};
use crate::{scan_progress, system_time_millis, COALESCE_WINDOW_MS};

/// Entries between progress reports and cancellation checks. Frequent enough that cancel
/// feels immediate, rare enough that the sink is never the bottleneck.
const PROGRESS_EVERY: u64 = 4096;

/// Index built by walking directories.
#[derive(Debug, Clone)]
pub struct WalkIndex {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Roots to index. Unlike the MFT backend this cannot cover a whole volume cheaply, so
    /// it is scoped to the user's home unless told otherwise.
    roots: Vec<String>,
    /// Normalised (forward-slash, folded) substrings; any path containing one is skipped.
    exclusions: Vec<String>,
    stores: RwLock<Vec<EntryStore>>,
    state: Mutex<IndexState>,
    last_scan_millis: AtomicU64,
    next_id: AtomicU64,
}

impl Default for WalkIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl WalkIndex {
    /// Indexes the current user's home directory, with the default exclusions.
    pub fn new() -> Self {
        Self::with_roots(home_dir().into_iter().collect())
    }

    pub fn with_roots(roots: Vec<String>) -> Self {
        Self::with_roots_and_exclusions(roots, IndexSettings::default().exclusions)
    }

    /// `exclusions` follow [`IndexSettings::exclusions`]: substring matches against the
    /// path, written with either separator.
    pub fn with_roots_and_exclusions(roots: Vec<String>, exclusions: Vec<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                roots,
                exclusions: exclusions
                    .iter()
                    .map(|e| normalise_for_exclusion(e))
                    .filter(|e| !e.is_empty())
                    .collect(),
                stores: RwLock::new(Vec::new()),
                state: Mutex::new(IndexState::Degraded),
                last_scan_millis: AtomicU64::new(0),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn roots(&self) -> &[String] {
        &self.inner.roots
    }
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|h| !h.is_empty())
}

fn normalise_for_exclusion(s: &str) -> String {
    crate::store::fold_lower(&s.replace('\\', "/"))
}

/// Canonicalises roots (FSEvents reports `/private/var/...` for `/var/...`, so watcher
/// paths only line up with canonical roots) and drops any root nested inside another,
/// which would otherwise index — and return — the same files twice.
fn effective_roots(roots: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = roots
        .iter()
        .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r)))
        .collect();
    out.sort();
    out.dedup();
    let mut kept: Vec<PathBuf> = Vec::with_capacity(out.len());
    for r in out {
        if !kept.iter().any(|k| r.starts_with(k)) {
            kept.push(r);
        }
    }
    kept
}

impl Inner {
    fn excluded(&self, path: &Path) -> bool {
        if self.exclusions.is_empty() {
            return false;
        }
        let p = normalise_for_exclusion(&path.to_string_lossy());
        self.exclusions.iter().any(|e| p.contains(e.as_str()))
    }

    fn alloc_id(&self) -> EntryId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Walks one root into a fresh store. Unreadable entries are skipped, not fatal: a
    /// home directory routinely contains a few folders the user cannot list.
    fn build_store(
        &self,
        root: &Path,
        progress: &dyn ProgressSink,
        done: &mut u64,
    ) -> Result<EntryStore> {
        let md = std::fs::metadata(root)?;
        if !md.is_dir() {
            return Err(FxError::InvalidPath(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let mount = root.to_string_lossy().into_owned();
        let mut store = EntryStore::with_mount(mount.clone(), mount, std::path::MAIN_SEPARATOR);
        // Path-keyed change events need `(parent, name) -> child`; enabling it before the
        // walk lets `push` maintain it incrementally instead of rebuilding afterwards.
        store.enable_child_index();
        let root_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let root_ix = store.push(
            self.alloc_id(),
            NO_RECORD,
            &root_name,
            meta_of(&root_name, &md),
        );
        store.set_root(root_ix);
        self.walk_into(&mut store, root, root_ix, progress, done)?;
        store.shrink_to_fit();
        Ok(store)
    }

    /// Adds everything beneath `dir` (already in the store as `dir_ix`). Shared by the
    /// initial scan and by the watcher when a whole directory appears at once — a folder
    /// moved in from outside the root arrives as a single event.
    fn walk_into(
        &self,
        store: &mut EntryStore,
        dir: &Path,
        dir_ix: RecordIx,
        progress: &dyn ProgressSink,
        done: &mut u64,
    ) -> Result<()> {
        // walkdir yields a directory before its contents, so the parent of an entry at
        // depth `d` is whatever was last pushed at depth `d - 1`: a stack replaces a
        // path -> record map entirely.
        let mut stack: Vec<RecordIx> = vec![dir_ix];
        let walker = walkdir::WalkDir::new(dir)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !self.excluded(e.path()));
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::debug!(%err, "walk: skipping unreadable entry");
                    continue;
                }
            };
            let depth = entry.depth();
            stack.truncate(depth);
            let Some(&parent) = stack.last() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy();
            let meta = match entry.metadata() {
                Ok(md) => meta_of(&name, &md),
                Err(_) => {
                    let kind = if entry.file_type().is_dir() {
                        EntryKind::Dir
                    } else {
                        EntryKind::Unknown
                    };
                    Meta::new(kind, &Attributes::default(), 0, 0, 0)
                }
            };
            let ix = store.push(self.alloc_id(), parent, &name, meta);
            if entry.file_type().is_dir() {
                stack.push(ix);
            }

            *done += 1;
            if (*done).is_multiple_of(PROGRESS_EVERY) {
                if !progress.should_continue() {
                    return Err(FxError::Cancelled);
                }
                progress.emit(&scan_progress(
                    OpState::Running,
                    *done,
                    0,
                    entry.path().to_string_lossy().into_owned(),
                ));
            }
        }
        Ok(())
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        let started = Instant::now();
        *self.state.lock() = IndexState::Scanning;
        let roots = effective_roots(&self.roots);
        let mut done = 0u64;
        let mut built = Vec::with_capacity(roots.len());
        let mut first_err = None;
        for root in &roots {
            match self.build_store(root, progress, &mut done) {
                Ok(s) => built.push(s),
                Err(FxError::Cancelled) => {
                    *self.state.lock() = IndexState::Degraded;
                    return Err(FxError::Cancelled);
                }
                Err(e) => {
                    tracing::warn!(root = %root.display(), error = %e, "walk: root skipped");
                    first_err.get_or_insert(e);
                }
            }
        }
        if built.is_empty() {
            if let Some(e) = first_err {
                *self.state.lock() = IndexState::Failed;
                return Err(e);
            }
        }
        // Built off-lock, swapped in at once: queries keep answering from the old index
        // for the whole scan instead of blocking on it.
        *self.stores.write() = built;
        *self.state.lock() = IndexState::Degraded;
        self.last_scan_millis
            .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        progress.emit(&scan_progress(OpState::Done, done, done, String::new()));
        Ok(())
    }

    /// Applies one coalesced batch of path changes, returning the events to report.
    fn apply(&self, changes: Vec<PathChange>) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        let mut touched: Vec<PathBuf> = Vec::new();
        let mut rescan: Vec<PathBuf> = Vec::new();
        {
            let mut stores = self.stores.write();
            for c in changes {
                match c {
                    PathChange::Renamed(from, to) => {
                        match rename_in(&mut stores, &from, &to, self) {
                            Some(ev) => events.push(ev),
                            None => {
                                touched.push(from);
                                touched.push(to);
                            }
                        }
                    }
                    PathChange::Touched(p) => touched.push(p),
                    PathChange::Rescan(p) => rescan.push(p),
                }
            }

            // Dedupe, then parents before children: a new directory must exist in the
            // store before anything created inside it can be attached.
            let mut seen = HashSet::new();
            touched.retain(|p| seen.insert(p.clone()));
            touched.sort_by_key(|p| p.components().count());
            for p in touched {
                if let Some((si, rel)) = locate(&stores, &p) {
                    self.refresh(&mut stores[si], &p, &rel, &mut events);
                }
            }
        }

        // The platform dropped events (FSEvents "must scan subdirs", inotify overflow):
        // deltas are lost, so the affected root is rebuilt and reported as desynced rather
        // than left silently wrong.
        let mut redone = HashSet::new();
        for p in rescan {
            let root = {
                let stores = self.stores.read();
                locate(&stores, &p).map(|(si, _)| (si, PathBuf::from(&stores[si].mount)))
            };
            let Some((si, root)) = root else { continue };
            if !redone.insert(si) {
                continue;
            }
            let mut done = 0;
            match self.build_store(&root, &fx_core::NullSink, &mut done) {
                Ok(fresh) => {
                    let volume = fresh.volume.clone();
                    let mut stores = self.stores.write();
                    if si < stores.len() {
                        stores[si] = fresh;
                    }
                    events.push(ChangeEvent::Desynced { volume });
                }
                Err(e) => tracing::warn!(root = %root.display(), error = %e, "walk: rescan failed"),
            }
        }
        events
    }

    /// Re-stats one path and brings the store in line with what is on disk.
    fn refresh(
        &self,
        store: &mut EntryStore,
        path: &Path,
        rel: &[String],
        out: &mut Vec<ChangeEvent>,
    ) {
        if rel.is_empty() {
            return; // The root itself: never a result, nothing to update.
        }
        let existing = store.lookup(rel.iter().map(String::as_str));
        let md = std::fs::symlink_metadata(path);
        let gone = md.is_err() || self.excluded(path);
        let volume = store.volume.clone();
        match (existing, md) {
            (Some(ix), _) if gone => {
                let id = store.id(ix);
                if store.record(ix).is_container() {
                    store.remove_subtree(id);
                } else {
                    store.remove(id);
                }
                out.push(ChangeEvent::Deleted { volume, id });
            }
            (Some(ix), Ok(md)) => {
                let id = store.id(ix);
                let was_dir = store.record(ix).is_container();
                let meta = meta_of(&rel[rel.len() - 1], &md);
                if store.record(ix).flags == meta.kind_and_attrs
                    && store.record(ix).size == meta.size
                    && store.record(ix).modified == meta.modified
                {
                    return; // A notification with no visible change (e.g. atime).
                }
                store.set_meta(id, meta);
                if was_dir && !md.is_dir() {
                    // A directory replaced by a file of the same name: its old contents
                    // are gone even though no event named them.
                    let children: Vec<EntryId> = (0..store.len() as RecordIx)
                        .filter(|&c| {
                            let r = store.record(c);
                            !r.is_tombstone() && r.parent == ix
                        })
                        .map(|c| store.id(c))
                        .collect();
                    for c in children {
                        store.remove_subtree(c);
                    }
                }
                out.push(ChangeEvent::Modified { volume, id });
            }
            (None, Ok(md)) if !gone => {
                let Some(parent) = self.ensure_dir(store, path.parent(), &rel[..rel.len() - 1])
                else {
                    return;
                };
                let name = &rel[rel.len() - 1];
                let id = self.alloc_id();
                let parent_id = store.id(parent);
                let (ix, _) = store.upsert(id, parent_id, name, meta_of(name, &md));
                if md.is_dir() {
                    let mut done = 0;
                    // Errors here mean the directory vanished mid-walk; the next event for
                    // it will reconcile, so a partial walk is fine.
                    let _ = self.walk_into(store, path, ix, &fx_core::NullSink, &mut done);
                }
                out.push(ChangeEvent::Created { volume, id });
            }
            _ => {}
        }
    }

    /// Finds the directory at `rel`, creating any missing ancestors from disk. Events can
    /// arrive for a file whose freshly created parent was never reported on its own.
    fn ensure_dir(
        &self,
        store: &mut EntryStore,
        dir: Option<&Path>,
        rel: &[String],
    ) -> Option<RecordIx> {
        if rel.is_empty() {
            return store.root();
        }
        if let Some(ix) = store.lookup(rel.iter().map(String::as_str)) {
            return store.record(ix).is_container().then_some(ix);
        }
        let dir = dir?;
        let md = std::fs::metadata(dir).ok().filter(|m| m.is_dir())?;
        if self.excluded(dir) {
            return None;
        }
        let parent = self.ensure_dir(store, dir.parent(), &rel[..rel.len() - 1])?;
        let name = &rel[rel.len() - 1];
        let parent_id = store.id(parent);
        let (ix, _) = store.upsert(self.alloc_id(), parent_id, name, meta_of(name, &md));
        Some(ix)
    }
}

/// Applies a paired rename as one operation, so the entry keeps its id. `None` when the
/// pair cannot be applied directly (source unknown, destination in another root or
/// excluded), in which case the caller falls back to re-stating both paths.
fn rename_in(
    stores: &mut [EntryStore],
    from: &Path,
    to: &Path,
    inner: &Inner,
) -> Option<ChangeEvent> {
    let (si, rel_from) = locate(stores, from)?;
    let (sj, rel_to) = locate(stores, to)?;
    if si != sj || rel_from.is_empty() || rel_to.is_empty() || inner.excluded(to) {
        return None;
    }
    let store = &mut stores[si];
    let ix = store.lookup(rel_from.iter().map(String::as_str))?;
    let parent = store.lookup(rel_to[..rel_to.len() - 1].iter().map(String::as_str))?;
    if !store.record(parent).is_container() {
        return None;
    }
    let id = store.id(ix);
    let parent_id = store.id(parent);
    let new_name = rel_to[rel_to.len() - 1].clone();
    let old_name = store.rename(id, parent_id, &new_name)?;
    Some(ChangeEvent::Renamed {
        volume: store.volume.clone(),
        id,
        old_name,
        new_name,
    })
}

/// The store whose root contains `path` (the deepest, should roots ever nest), and the
/// path's components below that root.
fn locate(stores: &[EntryStore], path: &Path) -> Option<(usize, Vec<String>)> {
    stores
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let rel = path.strip_prefix(&s.mount).ok()?;
            let comps: Option<Vec<String>> = rel
                .components()
                .map(|c| match c {
                    Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                    Component::CurDir => None,
                    _ => None,
                })
                .collect();
            Some((i, s.mount.len(), comps?))
        })
        .max_by_key(|(_, len, _)| *len)
        .map(|(i, _, c)| (i, c))
}

/// Everything about an entry except its name and position, from `lstat` metadata.
pub(crate) fn meta_of(name: &str, md: &std::fs::Metadata) -> Meta {
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Unknown
    };
    let attrs = attributes_of(name, md);
    let size = if kind == EntryKind::File { md.len() } else { 0 };
    let modified = md.modified().map(system_time_millis).unwrap_or(0);
    // Not every filesystem records a birth time; `0` is the contract's "unknown".
    let created = md.created().map(system_time_millis).unwrap_or(0);
    Meta::new(kind, &attrs, size, modified, created)
}

#[cfg(windows)]
fn attributes_of(_name: &str, md: &std::fs::Metadata) -> Attributes {
    use std::os::windows::fs::MetadataExt;
    attributes_from_win32(md.file_attributes())
}

#[cfg(not(windows))]
fn attributes_of(name: &str, md: &std::fs::Metadata) -> Attributes {
    // Unix has no hidden bit: a leading dot is the convention every file manager honours.
    Attributes {
        hidden: name.starts_with('.'),
        readonly: md.permissions().readonly(),
        reparse: md.file_type().is_symlink(),
        ..Default::default()
    }
}

/// Expands Win32 `FILE_ATTRIBUTE_*` bits. Shared with the MFT backend, which reads the same
/// bits out of `$STANDARD_INFORMATION`.
pub fn attributes_from_win32(a: u32) -> Attributes {
    Attributes {
        readonly: a & 0x1 != 0,
        hidden: a & 0x2 != 0,
        system: a & 0x4 != 0,
        archive: a & 0x20 != 0,
        sparse: a & 0x200 != 0,
        reparse: a & 0x400 != 0,
        compressed: a & 0x800 != 0,
        encrypted: a & 0x4000 != 0,
    }
}

/// A change as the watcher reports it, before re-stating.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathChange {
    Touched(PathBuf),
    Renamed(PathBuf, PathBuf),
    /// The platform lost events under this path; its root must be rebuilt.
    Rescan(PathBuf),
}

fn changes_from(event: notify::Event) -> Vec<PathChange> {
    use notify::event::{ModifyKind, RenameMode};
    use notify::EventKind;
    if event.need_rescan() {
        return event.paths.into_iter().map(PathChange::Rescan).collect();
    }
    match event.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            let mut p = event.paths.into_iter();
            match (p.next(), p.next()) {
                (Some(a), Some(b)) => vec![PathChange::Renamed(a, b)],
                _ => Vec::new(),
            }
        }
        _ => event.paths.into_iter().map(PathChange::Touched).collect(),
    }
}

impl IndexBackend for WalkIndex {
    fn stats(&self) -> IndexStats {
        let stores = self.inner.stores.read();
        IndexStats {
            // `Degraded` whenever usable: this backend is by definition not the fast path,
            // and the UI uses that to explain why search covers less than the whole machine.
            state: *self.inner.state.lock(),
            total_entries: stores.iter().map(|s| s.live_count() as u64).sum(),
            total_dirs: stores.iter().map(|s| s.live_dirs() as u64).sum(),
            memory_bytes: stores.iter().map(|s| s.memory_bytes()).sum(),
            last_scan_millis: self.inner.last_scan_millis.load(Ordering::Relaxed),
            volumes: stores.iter().map(|s| s.volume.clone()).collect(),
            helper_connected: false,
        }
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        self.inner.scan(progress)
    }

    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
        let stores = self.inner.stores.read();
        let refs: Vec<&EntryStore> = stores.iter().collect();
        execute(&refs, query, sink, true)
    }

    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle> {
        use notify::{RecursiveMode, Watcher};

        let mounts: Vec<String> = self
            .inner
            .stores
            .read()
            .iter()
            .map(|s| s.mount.clone())
            .collect();
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            // The receiver is gone only after the handle stopped; nothing left to tell.
            let _ = tx.send(res);
        })
        .map_err(|e| FxError::Index(format!("cannot start watcher: {e}")))?;
        for m in &mounts {
            watcher
                .watch(Path::new(m), RecursiveMode::Recursive)
                .map_err(|e| FxError::Index(format!("cannot watch {m}: {e}")))?;
        }

        let inner = Arc::clone(&self.inner);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("fx-walk-watch".into())
            .spawn(move || coalesce_loop(&inner, &rx, &stop_thread, &on_change))
            .map_err(|e| FxError::Index(format!("cannot spawn watcher thread: {e}")))?;

        Ok(WatchHandle::new(move || {
            stop.store(true, Ordering::Relaxed);
            // Dropping the watcher drops the sender, which wakes the loop with a
            // disconnect even if no event is pending.
            drop(watcher);
        }))
    }

    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String> {
        let stores = self.inner.stores.read();
        // Ids are unique across roots, so the volume only narrows the search; a caller that
        // passes a drive letter or `/` still finds entries in path-keyed stores.
        stores
            .iter()
            .filter(|s| volume.is_empty() || store_on_volume(s, volume))
            .find_map(|s| s.find(id).map(|ix| s.path_of(ix)))
            .ok_or_else(|| FxError::NotFound(format!("entry {id} on {volume}")))
    }

    fn clear(&self) -> Result<()> {
        *self.inner.stores.write() = Vec::new();
        Ok(())
    }
}

/// Collects notifications for [`COALESCE_WINDOW_MS`] after the first one arrives, then
/// applies them as one batch and makes one callback. Exits when the handle is stopped.
fn coalesce_loop(
    inner: &Inner,
    rx: &mpsc::Receiver<notify::Result<notify::Event>>,
    stop: &AtomicBool,
    on_change: &ChangeCallback,
) {
    let window = Duration::from_millis(COALESCE_WINDOW_MS);
    while !stop.load(Ordering::Relaxed) {
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        let deadline = Instant::now() + window;
        let mut pending = Vec::new();
        let mut take = |res: notify::Result<notify::Event>| match res {
            Ok(ev) => pending.extend(changes_from(ev)),
            Err(e) => {
                tracing::warn!(error = %e, "walk watcher error; scheduling rescan");
                pending.extend(e.paths.into_iter().map(PathChange::Rescan));
            }
        };
        take(first);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(ev) => take(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let events = inner.apply(pending);
        if !events.is_empty() {
            on_change(events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::{CollectSink, NullSink};
    use std::fs;

    /// A scratch directory removed on drop. Canonicalised so paths compare equal to what
    /// the backend stores (macOS `/var` is a symlink to `/private/var`).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "fx-walk-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(fs::canonicalize(&p).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn s(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// root/
    ///   docs/report.pdf
    ///   docs/notes.txt
    ///   src/main.rs
    ///   node_modules/pkg/index.js   (excluded by default)
    ///   .hidden
    fn sample() -> TempDir {
        let t = TempDir::new("sample");
        let p = t.path();
        fs::create_dir_all(p.join("docs")).unwrap();
        fs::create_dir_all(p.join("src")).unwrap();
        fs::create_dir_all(p.join("node_modules/pkg")).unwrap();
        fs::write(p.join("docs/report.pdf"), b"12345").unwrap();
        fs::write(p.join("docs/notes.txt"), b"n").unwrap();
        fs::write(p.join("src/main.rs"), b"fn main() {}").unwrap();
        fs::write(p.join("node_modules/pkg/index.js"), b"").unwrap();
        fs::write(p.join(".hidden"), b"").unwrap();
        mark_hidden(&p.join(".hidden"));
        t
    }

    /// Unix hides dotfiles by name, so `.hidden` already is. Windows hides by attribute.
    #[cfg(unix)]
    fn mark_hidden(_path: &Path) {}

    #[cfg(windows)]
    fn mark_hidden(path: &Path) {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
        // SAFETY: the HSTRING outlives the call and the attribute constant is a plain flag.
        #[allow(unsafe_code)]
        unsafe { SetFileAttributesW(&HSTRING::from(path.as_os_str()), FILE_ATTRIBUTE_HIDDEN) }
            .unwrap();
    }

    fn query(ix: &WalkIndex, text: &str) -> Vec<SearchHit> {
        let sink = CollectSink::new();
        let sum = ix
            .query(
                &Query {
                    text: text.into(),
                    ..Default::default()
                },
                &sink,
            )
            .unwrap();
        assert!(sum.degraded, "walk answers are always flagged degraded");
        sink.into_inner()
    }

    fn names(hits: &[SearchHit]) -> Vec<String> {
        let mut v: Vec<String> = hits.iter().map(|h| h.entry.name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn reports_degraded_so_the_ui_can_explain_itself() {
        let ix = WalkIndex::new();
        assert_eq!(ix.stats().state, IndexState::Degraded);
        assert!(!ix.stats().helper_connected);
    }

    #[test]
    fn clear_is_idempotent() {
        let ix = WalkIndex::with_roots(vec!["/tmp".into()]);
        assert!(ix.clear().is_ok());
        assert!(ix.clear().is_ok());
        assert_eq!(ix.stats().total_entries, 0);
    }

    #[test]
    fn scan_indexes_the_tree_and_honours_exclusions() {
        let t = sample();
        let ix = WalkIndex::with_roots(vec![t.s()]);
        ix.scan(&NullSink).unwrap();

        let stats = ix.stats();
        assert_eq!(stats.state, IndexState::Degraded);
        // root + docs + src + 3 files + .hidden; node_modules is excluded wholesale.
        assert_eq!(stats.total_entries, 7);
        assert_eq!(stats.total_dirs, 3);
        assert_eq!(stats.volumes, vec![t.s()]);
        assert!(stats.memory_bytes > 0);

        let hits = query(&ix, "report");
        assert_eq!(names(&hits), vec!["report.pdf"]);
        let h = &hits[0];
        assert_eq!(h.entry.size, 5);
        assert_eq!(h.entry.ext.as_deref(), Some("pdf"));
        assert!(h.entry.modified > 0);
        assert_eq!(
            PathBuf::from(&h.path),
            t.path().join("docs").join("report.pdf")
        );
        assert!(
            query(&ix, "index.js").is_empty(),
            "excluded subtree is not indexed"
        );
        assert!(
            query(&ix, "hidden").is_empty(),
            "dotfiles are hidden by default"
        );
    }

    #[test]
    fn exclusions_can_be_disabled() {
        let t = sample();
        let ix = WalkIndex::with_roots_and_exclusions(vec![t.s()], Vec::new());
        ix.scan(&NullSink).unwrap();
        assert_eq!(names(&query(&ix, "index")), vec!["index.js"]);
    }

    #[test]
    fn resolve_path_round_trips_ids_from_results() {
        let t = sample();
        let ix = WalkIndex::with_roots(vec![t.s()]);
        ix.scan(&NullSink).unwrap();
        let hit = query(&ix, "main.rs").remove(0);
        assert_eq!(
            ix.resolve_path(&hit.volume, hit.entry.id).unwrap(),
            hit.path
        );
        // Volume is only a narrowing hint; an empty one searches every root.
        assert_eq!(
            ix.resolve_path(&String::new(), hit.entry.id).unwrap(),
            hit.path
        );
        assert!(matches!(
            ix.resolve_path(&hit.volume, 999_999),
            Err(FxError::NotFound(_))
        ));
    }

    #[test]
    fn ids_are_never_reused_across_rescans() {
        let t = sample();
        let ix = WalkIndex::with_roots(vec![t.s()]);
        ix.scan(&NullSink).unwrap();
        let old = query(&ix, "main.rs").remove(0);
        ix.scan(&NullSink).unwrap();
        let new = query(&ix, "main.rs").remove(0);
        assert_ne!(old.entry.id, new.entry.id);
        assert!(ix.resolve_path(&old.volume, old.entry.id).is_err());
    }

    #[test]
    fn nested_and_duplicate_roots_are_indexed_once() {
        let t = sample();
        let ix = WalkIndex::with_roots(vec![
            t.s(),
            t.path().join("docs").to_string_lossy().into_owned(),
            t.s(),
        ]);
        ix.scan(&NullSink).unwrap();
        assert_eq!(query(&ix, "report").len(), 1);
        assert_eq!(ix.stats().volumes.len(), 1);
    }

    #[test]
    fn missing_roots_are_skipped_but_all_missing_fails() {
        let t = sample();
        let ix = WalkIndex::with_roots(vec![t.s(), "/definitely/not/here".into()]);
        ix.scan(&NullSink).unwrap();
        assert_eq!(query(&ix, "report").len(), 1);

        let ix = WalkIndex::with_roots(vec!["/definitely/not/here".into()]);
        assert!(matches!(ix.scan(&NullSink), Err(FxError::NotFound(_))));
        assert_eq!(ix.stats().state, IndexState::Failed);
    }

    #[test]
    fn cancelling_progress_stops_the_scan_and_keeps_the_old_index() {
        struct Cancel;
        impl ProgressSink for Cancel {
            fn emit(&self, _p: &fx_core::Progress) {}
            fn should_continue(&self) -> bool {
                false
            }
        }
        let t = TempDir::new("cancel");
        for i in 0..(PROGRESS_EVERY + 10) {
            fs::write(t.path().join(format!("f{i}")), b"").unwrap();
        }
        let ix = WalkIndex::with_roots(vec![t.s()]);
        assert_eq!(ix.scan(&Cancel), Err(FxError::Cancelled));
        assert_eq!(
            ix.stats().total_entries,
            0,
            "a cancelled scan installs nothing"
        );
        assert_eq!(ix.stats().state, IndexState::Degraded);
    }

    #[test]
    fn progress_reports_counts_and_finishes_done() {
        struct Record(std::sync::Mutex<Vec<fx_core::Progress>>);
        impl ProgressSink for Record {
            fn emit(&self, p: &fx_core::Progress) {
                self.0.lock().unwrap().push(p.clone());
            }
        }
        let t = sample();
        let sink = Record(Default::default());
        WalkIndex::with_roots(vec![t.s()]).scan(&sink).unwrap();
        let all = sink.0.into_inner().unwrap();
        let last = all.last().unwrap();
        assert_eq!(last.state, OpState::Done);
        assert_eq!(last.op, crate::INDEX_SCAN_OP);
        assert_eq!(last.done_items, 6);
        assert_eq!(last.eta_secs, None);
    }

    // ---- change application, driven directly so it is deterministic ----

    fn scanned(t: &TempDir) -> WalkIndex {
        let ix = WalkIndex::with_roots(vec![t.s()]);
        ix.scan(&NullSink).unwrap();
        ix
    }

    fn touch(ix: &WalkIndex, p: PathBuf) -> Vec<ChangeEvent> {
        ix.inner.apply(vec![PathChange::Touched(p)])
    }

    #[test]
    fn created_files_and_directories_are_added_under_the_right_parent() {
        let t = sample();
        let ix = scanned(&t);
        let p = t.path().join("docs/new.md");
        fs::write(&p, b"x").unwrap();
        let ev = touch(&ix, p.clone());
        assert!(matches!(ev[..], [ChangeEvent::Created { .. }]));
        let hit = query(&ix, "new.md").remove(0);
        assert_eq!(PathBuf::from(hit.path), p);

        // A whole directory appearing at once (moved in from elsewhere) brings its contents.
        let d = t.path().join("moved");
        fs::create_dir_all(d.join("deep/er")).unwrap();
        fs::write(d.join("deep/er/leaf.txt"), b"").unwrap();
        touch(&ix, d);
        assert_eq!(names(&query(&ix, "leaf")), vec!["leaf.txt"]);
        // Replaying the same event is idempotent.
        assert!(touch(&ix, p)
            .iter()
            .all(|e| !matches!(e, ChangeEvent::Created { .. })));
        assert_eq!(query(&ix, "new.md").len(), 1);
    }

    #[test]
    fn events_for_files_in_unreported_new_directories_create_the_ancestors() {
        let t = sample();
        let ix = scanned(&t);
        fs::create_dir_all(t.path().join("a/b")).unwrap();
        fs::write(t.path().join("a/b/c.txt"), b"").unwrap();
        touch(&ix, t.path().join("a/b/c.txt"));
        let hit = query(&ix, "c.txt").remove(0);
        assert_eq!(PathBuf::from(hit.path), t.path().join("a/b/c.txt"));
        assert!(names(&query(&ix, "b")).contains(&"b".to_string()));
    }

    #[test]
    fn deleting_a_directory_removes_its_subtree() {
        let t = sample();
        let ix = scanned(&t);
        fs::remove_dir_all(t.path().join("docs")).unwrap();
        let ev = touch(&ix, t.path().join("docs"));
        assert!(matches!(ev[..], [ChangeEvent::Deleted { .. }]));
        assert!(query(&ix, "report").is_empty());
        assert!(query(&ix, "notes").is_empty());
        assert_eq!(names(&query(&ix, "main")), vec!["main.rs"]);
    }

    #[test]
    fn modifications_update_metadata_and_quiet_events_are_dropped() {
        let t = sample();
        let ix = scanned(&t);
        let p = t.path().join("src/main.rs");
        assert!(
            touch(&ix, p.clone()).is_empty(),
            "no visible change, no event"
        );
        fs::write(&p, b"fn main() { println!(); }").unwrap();
        let ev = touch(&ix, p);
        assert!(matches!(ev[..], [ChangeEvent::Modified { .. }]));
        assert_eq!(query(&ix, "main.rs")[0].entry.size, 25);
    }

    #[test]
    fn paired_renames_keep_the_id_and_report_both_names() {
        let t = sample();
        let ix = scanned(&t);
        let before = query(&ix, "notes").remove(0);
        let from = t.path().join("docs/notes.txt");
        let to = t.path().join("src/todo.txt");
        fs::rename(&from, &to).unwrap();
        let ev = ix.inner.apply(vec![PathChange::Renamed(from, to.clone())]);
        match &ev[..] {
            [ChangeEvent::Renamed {
                id,
                old_name,
                new_name,
                ..
            }] => {
                assert_eq!(*id, before.entry.id);
                assert_eq!(old_name, "notes.txt");
                assert_eq!(new_name, "todo.txt");
            }
            other => panic!("expected one rename, got {other:?}"),
        }
        let after = query(&ix, "todo").remove(0);
        assert_eq!(after.entry.id, before.entry.id);
        assert_eq!(PathBuf::from(after.path), to);
        assert!(query(&ix, "notes").is_empty());
    }

    #[test]
    fn unpaired_rename_halves_converge_by_restat() {
        // FSEvents reports each half separately; re-stating both must land in the same
        // place a paired rename would (minus id preservation).
        let t = sample();
        let ix = scanned(&t);
        let from = t.path().join("docs/notes.txt");
        let to = t.path().join("docs/renamed.txt");
        fs::rename(&from, &to).unwrap();
        ix.inner
            .apply(vec![PathChange::Touched(from), PathChange::Touched(to)]);
        assert!(query(&ix, "notes").is_empty());
        assert_eq!(names(&query(&ix, "renamed")), vec!["renamed.txt"]);
    }

    #[test]
    fn rescan_requests_rebuild_the_root_and_report_desync() {
        let t = sample();
        let ix = scanned(&t);
        fs::write(t.path().join("silent.txt"), b"").unwrap();
        let ev = ix.inner.apply(vec![
            PathChange::Rescan(t.path().join("src")),
            PathChange::Rescan(t.path().join("docs")),
        ]);
        assert_eq!(
            ev,
            vec![ChangeEvent::Desynced { volume: t.s() }],
            "one per root"
        );
        assert_eq!(names(&query(&ix, "silent")), vec!["silent.txt"]);
    }

    #[test]
    fn events_outside_every_root_or_in_exclusions_are_ignored() {
        let t = sample();
        let ix = scanned(&t);
        assert!(touch(&ix, PathBuf::from("/somewhere/else.txt")).is_empty());
        fs::write(t.path().join("node_modules/pkg/new.js"), b"").unwrap();
        assert!(touch(&ix, t.path().join("node_modules/pkg/new.js")).is_empty());
        assert!(query(&ix, "new.js").is_empty());
    }

    #[test]
    fn notify_events_map_to_path_changes() {
        use notify::event::{CreateKind, ModifyKind, RenameMode};
        use notify::{Event, EventKind};
        let a = PathBuf::from("/r/a");
        let b = PathBuf::from("/r/b");
        let both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(a.clone())
            .add_path(b.clone());
        assert_eq!(changes_from(both), vec![PathChange::Renamed(a.clone(), b)]);
        let create = Event::new(EventKind::Create(CreateKind::File)).add_path(a.clone());
        assert_eq!(changes_from(create), vec![PathChange::Touched(a.clone())]);
        let access =
            Event::new(EventKind::Access(notify::event::AccessKind::Any)).add_path(a.clone());
        assert!(changes_from(access).is_empty());
        let rescan = Event::new(EventKind::Other)
            .add_path(a.clone())
            .set_flag(notify::event::Flag::Rescan);
        assert_eq!(changes_from(rescan), vec![PathChange::Rescan(a)]);
    }

    #[test]
    fn win32_attribute_bits_expand() {
        let a = attributes_from_win32(0x2 | 0x4 | 0x400);
        assert!(a.hidden && a.system && a.reparse);
        assert!(!a.readonly && !a.archive);
        assert_eq!(attributes_from_win32(0), Attributes::default());
    }

    #[test]
    fn live_watcher_coalesces_real_filesystem_changes() {
        let t = sample();
        let ix = scanned(&t);
        let batches = Arc::new(std::sync::Mutex::new(Vec::<Vec<ChangeEvent>>::new()));
        let b2 = Arc::clone(&batches);
        let handle = ix
            .watch(Box::new(move |ev| b2.lock().unwrap().push(ev)))
            .unwrap();
        // Some backends (FSEvents) need a moment before the stream is live.
        std::thread::sleep(Duration::from_millis(300));
        for i in 0..50 {
            fs::write(t.path().join(format!("burst{i}.log")), b"").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while query(&ix, "burst").len() < 50 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        handle.stop();
        assert_eq!(query(&ix, "burst").len(), 50);
        let n = batches.lock().unwrap().len();
        assert!(
            (1..50).contains(&n),
            "50 writes coalesced into {n} callbacks"
        );
    }
}
