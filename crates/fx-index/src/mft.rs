//! NTFS Master File Table index backend. **Windows only.**
//!
//! This is the fast path and the core of the product.
//!
//! ## How it works
//!
//! Every file on an NTFS volume has a record in the `$MFT` metafile. Reading the raw volume
//! `\\.\C:` sequentially yields all of them in one pass — no directory traversal, no
//! per-file `stat`. A full scan of a few million files takes a handful of seconds, versus
//! minutes for a recursive walk.
//!
//! Records stream through [`ntfs_reader::MftScan::next_chunk`] (never `Mft::new`, which
//! would hold the whole table in memory at once) into a [`VolumeBuilder`], which links
//! parents once every record has been seen. The USN journal ([`crate::usn`]) then keeps
//! the store live, and a checkpoint (store snapshot + journal position, one atomic file)
//! lets a restart resume in well under a second instead of rescanning.
//!
//! ## Ordering that keeps the index exact
//!
//! The journal position is taken **before** the MFT read starts, and the journal is
//! replayed from it once the read finishes. Anything that changed while the scan was
//! running is therefore applied afterwards; anything the scan already saw is re-applied
//! harmlessly, because [`apply_journal`] is idempotent. Taking the position afterwards
//! would silently lose every change made during the scan.
//!
//! ## Elevation
//!
//! Opening a raw volume handle requires administrator rights. The main app is never
//! elevated, so this backend runs inside `fx-helper` and is reached over
//! [`fx_core::HELPER_PIPE_NAME`]. Without rights every entry point reports
//! [`FxError::NotElevated`], never a bare `AccessDenied` — the UI keys its one-time
//! "Enable instant search" prompt off that distinction.
//!
//! ## Correctness traps, and where each is handled
//!
//! - Reused MFT records: parent references keep their sequence number and are validated
//!   against the full id ([`crate::store::EntryStore::link_parents`]).
//! - Hard links and DOS 8.3 aliases: one name per file, Win32 preferred
//!   ([`crate::ntfs::pick_name`]).
//! - Unused records: `MftChunk::files` yields only live base records.
//! - Metafiles and everything under `$Extend`: dropped by [`VolumeBuilder`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use fx_core::{
    BatchSink, ChangeCallback, ChangeEvent, EntryId, FxError, IndexBackend, IndexState, IndexStats,
    OpState, ProgressSink, Query, QuerySummary, Result, SearchHit, VolumeId, WatchHandle,
};
use ntfs_reader::{MftScan, NtfsFileNamespace, NtfsReaderError, Volume};
use parking_lot::{Mutex, RwLock};

use crate::ntfs::{
    apply_journal, checkpoint_file_name, load_checkpoint, pick_name, save_checkpoint, Checkpoint,
    NameCandidate, Namespace, RawRecord, VolumeBuilder,
};
use crate::query::{execute, store_on_volume};
use crate::store::EntryStore;
use crate::usn::{JournalError, UsnJournal};
use crate::{scan_progress, COALESCE_WINDOW_MS};

/// How often a live index is checkpointed to disk while changes are flowing. A crash loses
/// at most this much journal progress, which the journal itself still holds, so the cost
/// of a lost checkpoint is a slightly longer catch-up, never a wrong index.
const PERSIST_EVERY: Duration = Duration::from_secs(300);

/// Rescan interval for volumes whose journal is disabled. Without a journal the only way
/// to notice changes is to look again; this keeps such a volume roughly current without
/// making the disk thrash.
const RESCAN_WITHOUT_JOURNAL: Duration = Duration::from_secs(15 * 60);

/// Raw device path `ntfs-reader` expects for a drive letter (`C`, `C:` or `C:\`).
pub fn device_path_for(volume: &str) -> String {
    format!(r"\\.\{}:", volume.trim_end_matches(['\\', '/', ':']))
}

/// Path the volume root rebuilds to, e.g. `C:\`.
fn mount_for(volume: &str) -> String {
    format!(r"{}:\", volume.trim_end_matches(['\\', '/', ':']))
}

/// Converts an `ntfs-reader` error at the boundary. Missing rights become
/// [`FxError::NotElevated`] — never `AccessDenied` — and a volume that is not NTFS becomes
/// [`FxError::UnsupportedFilesystem`], which callers answer by falling back to the walk
/// backend.
pub fn map_ntfs_error(e: NtfsReaderError, what: &str) -> FxError {
    match e {
        NtfsReaderError::AccessDenied => FxError::NotElevated(format!(
            "{what}: reading the raw volume requires administrator rights"
        )),
        NtfsReaderError::InvalidBootSector { .. } => {
            FxError::UnsupportedFilesystem(format!("{what}: not an NTFS volume"))
        }
        NtfsReaderError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            FxError::NotFound(format!("{what}: {io}"))
        }
        other => FxError::Index(format!("{what}: {other}")),
    }
}

/// Drive letters of every fixed NTFS volume. Removable and network drives are excluded:
/// they come and go, and the latter cannot be MFT-read at all.
#[allow(unsafe_code)]
pub fn ntfs_volumes() -> Vec<VolumeId> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    const DRIVE_FIXED: u32 = 3;

    // SAFETY: no arguments; returns a bitmask.
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            let root: Vec<u16> = format!(r"{letter}:\").encode_utf16().chain([0]).collect();
            let p = PCWSTR::from_raw(root.as_ptr());
            // SAFETY: `root` is NUL-terminated and outlives both calls; the output buffer
            // is a correctly sized slice the API writes into.
            if unsafe { GetDriveTypeW(p) } != DRIVE_FIXED {
                return None;
            }
            let mut fs = [0u16; 32];
            unsafe { GetVolumeInformationW(p, None, None, None, None, Some(&mut fs)) }.ok()?;
            let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
            (String::from_utf16_lossy(&fs[..len]) == "NTFS").then(|| letter.to_string())
        })
        .collect()
}

/// Index backed by raw NTFS volume reads, one store per volume.
#[derive(Debug, Clone)]
pub struct MftIndex {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    volumes: Vec<VolumeId>,
    /// Where checkpoints live. `None` disables persistence (tests, one-off use).
    state_dir: Option<PathBuf>,
    stores: RwLock<Vec<EntryStore>>,
    /// Journal position each volume's store is current to.
    checkpoints: Mutex<HashMap<VolumeId, Checkpoint>>,
    state: Mutex<IndexState>,
    last_scan_millis: AtomicU64,
}

impl MftIndex {
    /// `volume` is a drive letter without a colon, e.g. `"C"`.
    pub fn new(volume: impl Into<VolumeId>) -> Self {
        Self::with_volumes(vec![volume.into()])
    }

    pub fn with_volumes(volumes: Vec<VolumeId>) -> Self {
        Self {
            inner: Arc::new(Inner {
                volumes: volumes
                    .into_iter()
                    .map(|v| v.trim_end_matches(['\\', '/', ':']).to_ascii_uppercase())
                    .collect(),
                state_dir: None,
                stores: RwLock::new(Vec::new()),
                checkpoints: Mutex::new(HashMap::new()),
                state: Mutex::new(IndexState::Idle),
                last_scan_millis: AtomicU64::new(0),
            }),
        }
    }

    /// Every fixed NTFS volume on this machine.
    pub fn all_volumes() -> Self {
        Self::with_volumes(ntfs_volumes())
    }

    /// Persists checkpoints under `dir` (the helper uses `%ProgramData%\Fulcrum`). Must be
    /// called before the index is shared.
    pub fn with_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.state_dir = Some(dir.into());
        }
        self
    }

    pub fn volumes(&self) -> &[VolumeId] {
        &self.inner.volumes
    }

    /// Raw device path of the first volume.
    pub fn device_path(&self) -> String {
        device_path_for(self.inner.volumes.first().map_or("C", String::as_str))
    }

    /// Whether this process can open the raw volume. Lets the app decide between the MFT
    /// and walk backends without triggering a UAC prompt first.
    pub fn is_available(&self) -> bool {
        Volume::new(self.device_path()).is_ok()
    }

    /// Loads every volume from its checkpoint and catches up from the journal. Returns
    /// `true` only if every volume was restored; the caller then skips the full scan.
    /// Volumes that could not be restored (no checkpoint, journal wrapped or recreated)
    /// are left for [`IndexBackend::scan`].
    pub fn restore(&self) -> bool {
        self.inner.restore()
    }
}

impl Inner {
    fn checkpoint_path(&self, volume: &str) -> Option<PathBuf> {
        self.state_dir
            .as_ref()
            .map(|d| d.join(checkpoint_file_name(volume)))
    }

    fn persist(&self, volume: &str) {
        let Some(path) = self.checkpoint_path(volume) else {
            return;
        };
        let cp = self.checkpoints.lock().get(volume).copied();
        let stores = self.stores.read();
        if let Some(store) = stores.iter().find(|s| s.volume == volume) {
            if let Err(e) = save_checkpoint(&path, store, cp) {
                tracing::warn!(volume, error = %e, "could not save index checkpoint");
            }
        }
    }

    fn install(&self, store: EntryStore, cp: Option<Checkpoint>) {
        let volume = store.volume.clone();
        {
            let mut stores = self.stores.write();
            match stores.iter_mut().find(|s| s.volume == volume) {
                Some(slot) => *slot = store,
                None => stores.push(store),
            }
        }
        let mut cps = self.checkpoints.lock();
        match cp {
            Some(cp) => cps.insert(volume, cp),
            None => cps.remove(&volume),
        };
    }

    fn restore(&self) -> bool {
        let mut all = true;
        for volume in &self.volumes {
            let Some(path) = self.checkpoint_path(volume) else {
                return false;
            };
            let restored = load_checkpoint(&path).ok().and_then(|(store, cp)| {
                let cp = cp?;
                let mut journal = UsnJournal::open(volume, Some(cp)).ok()?;
                let mut store = store;
                catch_up(&mut store, &mut journal).ok()?;
                Some((store, journal.checkpoint()))
            });
            match restored {
                Some((store, cp)) => {
                    tracing::info!(
                        volume,
                        entries = store.live_count(),
                        "index restored from checkpoint"
                    );
                    self.install(store, Some(cp));
                }
                None => all = false,
            }
        }
        if all && !self.volumes.is_empty() {
            *self.state.lock() = IndexState::Ready;
        }
        all
    }

    /// Reads one volume's MFT into a fresh store, then replays the journal from a position
    /// taken before the read began (see the module docs for why that order matters).
    fn scan_volume(
        &self,
        volume: &str,
        progress: &dyn ProgressSink,
    ) -> Result<(EntryStore, Option<Checkpoint>)> {
        let device = device_path_for(volume);
        let what = format!("volume {volume}");

        let mut journal = match UsnJournal::open(&volume.to_owned(), None) {
            Ok(j) => Some(j),
            Err(JournalError::Failed(e @ FxError::NotElevated(_))) => return Err(e),
            Err(e) => {
                tracing::warn!(volume, error = ?e, "no usable change journal; index will not stay live");
                None
            }
        };

        let v = Volume::new(&device).map_err(|e| map_ntfs_error(e, &what))?;
        let mut scan = MftScan::new(v).map_err(|e| map_ntfs_error(e, &what))?;
        let total = scan.record_count();
        let mut b = VolumeBuilder::new(volume.to_owned(), mount_for(volume));
        let mut last_emit = Instant::now();

        while let Some(chunk) = scan.next_chunk().map_err(|e| map_ntfs_error(e, &what))? {
            for file in chunk.files() {
                if let Some(raw) = raw_record(&file) {
                    b.push(raw);
                }
            }
            if !progress.should_continue() {
                return Err(FxError::Cancelled);
            }
            if last_emit.elapsed() >= Duration::from_millis(100) {
                last_emit = Instant::now();
                progress.emit(&scan_progress(
                    OpState::Running,
                    chunk.records().end,
                    total,
                    format!(r"{volume}:\"),
                ));
            }
        }
        let mut store = b.finish();

        let cp = match journal.as_mut() {
            Some(j) => match catch_up(&mut store, j) {
                Ok(()) => Some(j.checkpoint()),
                // Wrapped during the scan itself: the journal is too small for this
                // volume's churn. The fresh store is still the best answer available.
                Err(e) => {
                    tracing::warn!(volume, error = ?e, "journal lost during scan");
                    None
                }
            },
            None => None,
        };
        Ok((store, cp))
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        let started = Instant::now();
        let prev = std::mem::replace(&mut *self.state.lock(), IndexState::Scanning);
        let mut ok = 0usize;
        let mut first_err = None;
        for volume in &self.volumes {
            match self.scan_volume(volume, progress) {
                Ok((store, cp)) => {
                    tracing::info!(volume, entries = store.live_count(), "volume indexed");
                    self.install(store, cp);
                    self.persist(volume);
                    ok += 1;
                }
                // Neither is per-volume: stop now so the UI can prompt / stay quiet.
                Err(e @ (FxError::NotElevated(_) | FxError::Cancelled)) => {
                    *self.state.lock() = prev;
                    return Err(e);
                }
                Err(e) => {
                    tracing::warn!(volume, error = %e, "volume skipped");
                    first_err.get_or_insert(e);
                }
            }
        }
        if ok == 0 {
            *self.state.lock() = IndexState::Failed;
            return Err(
                first_err.unwrap_or_else(|| FxError::Index("no NTFS volumes to index".into()))
            );
        }
        *self.state.lock() = IndexState::Ready;
        self.last_scan_millis
            .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        progress.emit(&scan_progress(OpState::Done, 0, 0, String::new()));
        Ok(())
    }

    /// Applies journal records in three phases so the store's write lock is never held
    /// across a filesystem call: structure under the lock, stats with no lock, then a
    /// brief second lock to install sizes and times.
    fn apply_live(&self, volume: &str, records: &[crate::ntfs::JournalRecord]) -> Vec<ChangeEvent> {
        let mut wanted: Vec<(EntryId, String)> = Vec::new();
        let events = {
            let mut stores = self.stores.write();
            let Some(store) = stores.iter_mut().find(|s| s.volume == volume) else {
                return Vec::new();
            };
            apply_journal(store, records, &mut |id, path| {
                wanted.push((id, path.to_owned()));
                None
            })
        };
        let metas: Vec<(EntryId, crate::store::Meta)> = wanted
            .into_iter()
            .filter_map(|(id, path)| {
                let md = std::fs::symlink_metadata(&path).ok()?;
                let name = path.rsplit('\\').next().unwrap_or("");
                Some((id, crate::walk::meta_of(name, &md)))
            })
            .collect();
        if !metas.is_empty() {
            let mut stores = self.stores.write();
            if let Some(store) = stores.iter_mut().find(|s| s.volume == volume) {
                for (id, meta) in metas {
                    store.set_meta(id, meta);
                }
            }
        }
        events
    }
}

/// Converts one live MFT file into what the index keeps. `None` for a record with no usable
/// name (an extension-only remnant).
fn raw_record(file: &ntfs_reader::NtfsFile<'_>) -> Option<RawRecord> {
    let names = file.names().filter_map(|n| {
        let namespace = match n.namespace()? {
            NtfsFileNamespace::Posix => Namespace::Posix,
            NtfsFileNamespace::Win32 => Namespace::Win32,
            NtfsFileNamespace::Dos => Namespace::Dos,
            NtfsFileNamespace::Win32AndDos => Namespace::Win32AndDos,
        };
        Some(NameCandidate {
            namespace,
            parent: n.parent_reference(),
            name: n.to_string(),
        })
    });
    let best = pick_name(names)?;
    let is_dir = file.is_directory();
    let si = file.standard_information();
    let size = if is_dir {
        0
    } else {
        file.data_streams()
            .find(|s| s.name.is_none())
            .map_or(0, |s| s.size)
    };
    Some(RawRecord {
        id: file.reference(),
        parent: best.parent,
        name: best.name,
        is_dir,
        size,
        attrs: si.map_or(0, |s| s.file_attributes()),
        // `ntfs-reader` hands back `time::OffsetDateTime`; calling its inherent method
        // inline avoids a direct dependency on `time` just to name the type.
        modified: si.map_or(0, |s| {
            (s.modified().unix_timestamp_nanos() / 1_000_000) as i64
        }),
        created: si.map_or(0, |s| {
            (s.created().unix_timestamp_nanos() / 1_000_000) as i64
        }),
    })
}

/// Drains the journal into `store` until it reports nothing more.
fn catch_up(
    store: &mut EntryStore,
    journal: &mut UsnJournal,
) -> std::result::Result<(), JournalError> {
    loop {
        let records = journal.read_available()?;
        if records.is_empty() {
            return Ok(());
        }
        apply_journal(store, &records, &mut |_, path| {
            let md = std::fs::symlink_metadata(path).ok()?;
            let name = path.rsplit('\\').next().unwrap_or("");
            Some(crate::walk::meta_of(name, &md))
        });
    }
}

impl IndexBackend for MftIndex {
    fn stats(&self) -> IndexStats {
        let stores = self.inner.stores.read();
        IndexStats {
            state: *self.inner.state.lock(),
            total_entries: stores.iter().map(|s| s.live_count() as u64).sum(),
            total_dirs: stores.iter().map(|s| s.live_dirs() as u64).sum(),
            memory_bytes: stores.iter().map(|s| s.memory_bytes()).sum(),
            last_scan_millis: self.inner.last_scan_millis.load(Ordering::Relaxed),
            volumes: self.inner.volumes.clone(),
            // Running in-process means this process holds the raw handle itself; the
            // pipe client reports the helper's own view instead.
            helper_connected: false,
        }
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        self.inner.scan(progress)
    }

    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
        let stores = self.inner.stores.read();
        let refs: Vec<&EntryStore> = stores.iter().collect();
        execute(&refs, query, sink, false)
    }

    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle> {
        let inner = Arc::clone(&self.inner);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_thread = Arc::clone(&stopped);
        std::thread::Builder::new()
            .name("fx-usn-watch".into())
            .spawn(move || watch_loop(&inner, &stop_rx, &stopped_thread, &on_change))
            .map_err(|e| FxError::Index(format!("cannot spawn journal thread: {e}")))?;
        Ok(WatchHandle::new(move || {
            stopped.store(true, Ordering::Relaxed);
            let _ = stop_tx.send(());
        }))
    }

    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String> {
        let stores = self.inner.stores.read();
        stores
            .iter()
            .filter(|s| volume.is_empty() || store_on_volume(s, volume))
            .find_map(|s| s.find(id).map(|ix| s.path_of(ix)))
            .ok_or_else(|| FxError::NotFound(format!("entry {id:#x} on {volume}")))
    }

    fn clear(&self) -> Result<()> {
        *self.inner.stores.write() = Vec::new();
        self.inner.checkpoints.lock().clear();
        *self.inner.state.lock() = IndexState::Idle;
        Ok(())
    }
}

/// Polls every volume's journal once per coalescing window and publishes one batch of
/// events per window. Polling (rather than a blocking read) is what does the coalescing:
/// everything that arrived during the window is read and applied together.
fn watch_loop(
    inner: &Inner,
    stop: &mpsc::Receiver<()>,
    stopped: &AtomicBool,
    on_change: &ChangeCallback,
) {
    let mut journals: HashMap<VolumeId, UsnJournal> = HashMap::new();
    let mut rescan_at: HashMap<VolumeId, Instant> = HashMap::new();
    for v in &inner.volumes {
        let cp = inner.checkpoints.lock().get(v).copied();
        match cp.map(|cp| UsnJournal::open(v, Some(cp))) {
            Some(Ok(j)) => {
                journals.insert(v.clone(), j);
            }
            Some(Err(JournalError::Desynced)) => {
                rescan_at.insert(v.clone(), Instant::now());
            }
            _ => {
                rescan_at.insert(v.clone(), Instant::now() + RESCAN_WITHOUT_JOURNAL);
            }
        }
    }
    let mut dirty: HashMap<VolumeId, bool> = HashMap::new();
    let mut last_persist = Instant::now();

    loop {
        match stop.recv_timeout(Duration::from_millis(COALESCE_WINDOW_MS)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        let mut events = Vec::new();

        for v in &inner.volumes {
            let Some(j) = journals.get_mut(v) else {
                continue;
            };
            match j.read_available() {
                Ok(records) if records.is_empty() => {}
                Ok(records) => {
                    events.extend(inner.apply_live(v, &records));
                    inner.checkpoints.lock().insert(v.clone(), j.checkpoint());
                    dirty.insert(v.clone(), true);
                }
                Err(JournalError::Desynced) => {
                    tracing::warn!(volume = %v, "usn journal wrapped or was recreated; rescanning");
                    journals.remove(v);
                    rescan_at.insert(v.clone(), Instant::now());
                }
                Err(JournalError::Unavailable(why)) => {
                    tracing::warn!(volume = %v, %why, "usn journal unavailable; falling back to periodic rescans");
                    journals.remove(v);
                    rescan_at.insert(v.clone(), Instant::now() + RESCAN_WITHOUT_JOURNAL);
                }
                Err(JournalError::Failed(e)) => {
                    // Transient (or a bug): keep the journal and retry next window.
                    tracing::warn!(volume = %v, error = %e, "usn read failed");
                }
            }
        }

        // Rebuild volumes whose deltas were lost. `Desynced` goes out first so the UI
        // knows any results it holds for the volume are stale.
        let due: Vec<VolumeId> = rescan_at
            .iter()
            .filter(|(_, at)| Instant::now() >= **at)
            .map(|(v, _)| v.clone())
            .collect();
        for v in due {
            rescan_at.remove(&v);
            match inner.scan_volume(&v, &fx_core::NullSink) {
                Ok((store, cp)) => {
                    inner.install(store, cp);
                    inner.persist(&v);
                    events.push(ChangeEvent::Desynced { volume: v.clone() });
                    match cp.map(|cp| UsnJournal::open(&v, Some(cp))) {
                        Some(Ok(j)) => {
                            journals.insert(v, j);
                        }
                        _ => {
                            rescan_at.insert(v, Instant::now() + RESCAN_WITHOUT_JOURNAL);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(volume = %v, error = %e, "rescan failed; will retry");
                    rescan_at.insert(v, Instant::now() + RESCAN_WITHOUT_JOURNAL);
                }
            }
        }

        if !events.is_empty() && !stopped.load(Ordering::Relaxed) {
            on_change(events);
        }
        if last_persist.elapsed() >= PERSIST_EVERY {
            last_persist = Instant::now();
            for (v, d) in dirty.iter_mut() {
                if std::mem::take(d) {
                    inner.persist(v);
                }
            }
        }
    }

    // A clean stop persists, so the next start resumes from here instead of rescanning.
    for (v, d) in dirty {
        if d {
            inner.persist(&v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_path_is_the_raw_volume_form() {
        assert_eq!(MftIndex::new("C").device_path(), r"\\.\C:");
        assert_eq!(device_path_for("D"), r"\\.\D:");
        assert_eq!(device_path_for(r"E:\"), r"\\.\E:");
        assert_eq!(mount_for("c"), r"c:\");
    }

    #[test]
    fn volume_ids_are_normalised() {
        let ix = MftIndex::with_volumes(vec!["c:".into(), r"D:\".into()]);
        assert_eq!(ix.volumes(), ["C", "D"]);
    }

    #[test]
    fn missing_rights_are_not_elevated_never_access_denied() {
        assert!(matches!(
            map_ntfs_error(NtfsReaderError::AccessDenied, "C"),
            FxError::NotElevated(_)
        ));
        assert!(matches!(
            map_ntfs_error(NtfsReaderError::InvalidBootSector { field: "oem_id" }, "C"),
            FxError::UnsupportedFilesystem(_)
        ));
    }
}
