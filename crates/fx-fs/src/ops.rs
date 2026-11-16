//! Copy, move, delete — the operation engine.
//!
//! Portable logic, tested on macOS.
//!
//! ## Two-phase execution
//!
//! Each operation scans before it acts, so the progress bar has a real denominator from the
//! first byte. See the crate docs for why this matters. When `verify` is on, the byte total
//! counts the verification pass too, so the bar does not sit at 100% while re-reading.
//!
//! ## Threading model
//!
//! [`FsBackend::start_op`](fx_core::FsBackend::start_op) receives its [`ProgressSink`] by
//! reference, so the sink cannot outlive the call and the work cannot be moved to a thread
//! of our own. The operation therefore runs **on the calling thread** — the shell already
//! calls this inside `spawn_blocking` — and [`OpQueue::start`] returns when it finishes.
//! The UI learns the [`OpId`] from the very first [`Progress`] event (the `Scanning` one),
//! which is emitted before any work, and controls the job concurrently through
//! [`OpQueue::control`] from another thread.
//!
//! ## Conflicts
//!
//! With [`ConflictPolicy::Ask`] the job parks in [`OpState::Paused`] with
//! [`Progress::pending_conflict`] set and waits for [`OpControl::Resolve`]. The
//! `apply_to_all` flag exists so a 10,000-file copy asks once, not ten thousand times.
//! While parked, the paused snapshot is re-emitted periodically, so a rate-limiting sink
//! that happened to drop the first one cannot leave the UI unaware that it is being asked.
//!
//! Directories never conflict: copying `photos` onto an existing `photos` merges them, and
//! only the files inside can conflict — which is what Explorer does. The exception is
//! [`ConflictPolicy::Rename`], which keeps both as `photos (2)`.
//!
//! ## Cancellation
//!
//! Cooperative: [`ProgressSink::should_continue`] and the job's cancel flag are checked
//! between chunks, not just between files, or cancelling during a 40 GB copy would do
//! nothing for minutes. Copies are written to a `.name.fxpart` file and renamed into place
//! only once complete (and verified), so a cancelled or failed copy never leaves a
//! truncated file under the real name — and an overwrite never destroys the original
//! before its replacement is whole.

use fx_core::{
    Conflict, ConflictPolicy, FsOpKind, FsOpRequest, FxError, OpControl, OpError, OpId, OpState,
    Progress, ProgressSink, Result,
};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Chunk size for copy loops.
///
/// 1 MiB is large enough to keep sequential throughput near the device limit while still
/// allowing cancellation to be noticed promptly.
pub const COPY_CHUNK: usize = 1024 * 1024;

/// Minimum gap between routine progress emissions. State changes bypass it. The IPC
/// boundary throttles again (to ~10/sec); this just avoids building snapshots per chunk
/// that would be thrown away.
const EMIT_INTERVAL: Duration = Duration::from_millis(50);

/// How often a parked job (paused, or waiting on a conflict) re-announces itself.
const PARKED_REEMIT: Duration = Duration::from_millis(250);

/// Throughput is measured over windows of this length and smoothed.
const RATE_WINDOW: Duration = Duration::from_millis(500);

/// No ETA until the transfer has run this long. The first second of a copy is dominated
/// by cache effects and file-open latency; an estimate from it would be a guess.
const ETA_WARMUP: Duration = Duration::from_secs(1);

/// Per-file errors kept verbatim. Past this, they are counted and summarised: a copy of
/// 100k files to a full disk must not carry 100k strings in every progress snapshot.
const MAX_ERRORS: usize = 500;

/// Suffix for in-flight copies. See the module docs.
const PARTIAL_SUFFIX: &str = ".fxpart";

/// Tracks running operations and routes control messages to them.
#[derive(Debug, Default)]
pub struct OpQueue {
    next_id: AtomicU64,
    jobs: Mutex<HashMap<OpId, Arc<JobControl>>>,
}

impl OpQueue {
    pub fn new() -> Self {
        Self::default()
    }

    fn allocate(&self) -> OpId {
        OpId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Runs an operation to completion on the calling thread, reporting through
    /// `progress`. See the module docs for why this does not return early.
    ///
    /// Returns the id on success — including when some items failed, which are reported
    /// in [`Progress::errors`]. Returns [`FxError::Cancelled`] if cancelled, and an error
    /// if the operation could not proceed at all or every item failed.
    pub fn start(&self, request: FsOpRequest, progress: &dyn ProgressSink) -> Result<OpId> {
        validate(&request)?;
        let id = self.allocate();
        let control = Arc::new(JobControl::default());
        self.jobs.lock().insert(id, Arc::clone(&control));
        // Deregister however the job ends, including by panic.
        let _registered = Registration { queue: self, id };
        Job::new(id, &request, progress, &control)
            .run()
            .map(|()| id)
    }

    pub fn control(&self, op: OpId, control: OpControl) -> Result<()> {
        let job = self
            .jobs
            .lock()
            .get(&op)
            .cloned()
            .ok_or_else(|| FxError::NotFound(format!("operation {} is not running", op.0)))?;
        job.apply(control)
    }

    /// Number of operations currently running.
    pub fn active(&self) -> usize {
        self.jobs.lock().len()
    }
}

struct Registration<'a> {
    queue: &'a OpQueue,
    id: OpId,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.queue.jobs.lock().remove(&self.id);
    }
}

/// The control surface of one running job, shared between the worker and whoever calls
/// [`OpQueue::control`].
#[derive(Debug, Default)]
struct JobControl {
    state: Mutex<ControlState>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct ControlState {
    paused: bool,
    cancelled: bool,
    /// The worker is parked on a conflict; only then is `Resolve` meaningful.
    awaiting_conflict: bool,
    resolution: Option<(ConflictPolicy, bool)>,
}

impl JobControl {
    fn apply(&self, control: OpControl) -> Result<()> {
        let mut s = self.state.lock();
        match control {
            OpControl::Pause => s.paused = true,
            OpControl::Resume => s.paused = false,
            OpControl::Cancel => s.cancelled = true,
            OpControl::Resolve {
                policy,
                apply_to_all,
            } => {
                if policy == ConflictPolicy::Ask {
                    return Err(FxError::BadQuery(
                        "a conflict must be resolved with a concrete policy, not Ask".into(),
                    ));
                }
                if !s.awaiting_conflict {
                    return Err(FxError::BadQuery("no conflict is pending".into()));
                }
                s.resolution = Some((policy, apply_to_all));
            }
        }
        self.wake.notify_all();
        Ok(())
    }
}

/// Rejects malformed requests before any work begins.
pub fn validate(request: &FsOpRequest) -> Result<()> {
    if request.sources.is_empty() {
        return Err(FxError::BadQuery("operation has no sources".into()));
    }
    match request.kind {
        FsOpKind::Copy | FsOpKind::Move => {
            let Some(dest) = request.destination.as_deref() else {
                return Err(FxError::BadQuery("copy/move requires a destination".into()));
            };
            // Copying a directory into itself is an infinite recursion, and users do try
            // it by dragging onto a breadcrumb.
            for src in &request.sources {
                if dest == src || dest.starts_with(&format!("{src}/")) || is_win_subpath(dest, src)
                {
                    return Err(FxError::InvalidPath(format!(
                        "cannot move {src} into its own subdirectory"
                    )));
                }
            }
            Ok(())
        }
        FsOpKind::Delete | FsOpKind::Trash => Ok(()),
    }
}

fn is_win_subpath(dest: &str, src: &str) -> bool {
    dest.to_lowercase()
        .starts_with(&format!("{}\\", src.to_lowercase()))
}

/// Renames in place, keeping the parent directory.
pub fn rename(path: &str, new_name: &str) -> Result<()> {
    if new_name.is_empty() || new_name.contains(['/', '\\']) {
        return Err(FxError::InvalidPath(format!("invalid name: {new_name}")));
    }
    let p = Path::new(path);
    let parent = p
        .parent()
        .ok_or_else(|| FxError::InvalidPath(path.into()))?;
    let target = parent.join(new_name);
    // A case-only rename (`readme` → `README`) "exists" on case-insensitive filesystems
    // but is the same file, and must be allowed.
    if target.exists() && !is_same_entry(p, &target) {
        return Err(FxError::AlreadyExists(
            target.to_string_lossy().into_owned(),
        ));
    }
    fs::rename(p, target).map_err(Into::into)
}

/// Picks a non-colliding name the way Explorer does: `report (2).pdf`.
pub fn disambiguate(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = fx_core::FileEntry::split_ext(name);
    for n in 2..10_000 {
        let candidate = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let path = dir.join(candidate);
        // symlink_metadata, so a dangling symlink still counts as taken.
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
    dir.join(name)
}

/// An initial progress snapshot in the scanning phase, before totals are known.
pub fn initial_progress(op: OpId, kind: FsOpKind) -> Progress {
    Progress {
        op,
        kind,
        state: OpState::Scanning,
        total_bytes: 0,
        done_bytes: 0,
        total_items: 0,
        done_items: 0,
        current: String::new(),
        bytes_per_sec: 0,
        // No estimate until the scan has produced a denominator.
        eta_secs: None,
        errors: Vec::new(),
        pending_conflict: None,
    }
}

/// Remaining seconds, or `None` when not yet meaningful.
pub fn estimate_eta(done_bytes: u64, total_bytes: u64, bytes_per_sec: u64) -> Option<u64> {
    if bytes_per_sec == 0 || total_bytes == 0 || done_bytes > total_bytes {
        return None;
    }
    Some((total_bytes - done_bytes) / bytes_per_sec)
}

// ---- plan -------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Dir,
    File,
    Symlink,
}

impl NodeKind {
    fn of(meta: &fs::Metadata) -> Self {
        let ft = meta.file_type();
        if ft.is_symlink() {
            NodeKind::Symlink
        } else if ft.is_dir() {
            NodeKind::Dir
        } else {
            NodeKind::File
        }
    }
}

/// One entry found by the scan, relative to its source root (the root itself has an
/// empty `rel`). Parents always precede their children.
#[derive(Debug)]
struct Node {
    rel: PathBuf,
    kind: NodeKind,
    size: u64,
}

/// One top-level source and everything under it.
#[derive(Debug)]
struct Root {
    src: PathBuf,
    nodes: Vec<Node>,
    /// Handled as a single unit without walking: a same-volume move (one rename) or a
    /// trash (one shell call).
    whole: bool,
}

/// `base.join(rel)` without the trailing separator `join("")` would add.
fn under(base: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rel)
    }
}

/// Outcome of consulting the conflict policy for one file.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Proceed,
    Skip,
    RenameTo(PathBuf),
}

// ---- the job ----------------------------------------------------------------------------

/// Smoothed throughput, measured only while actually running so a pause or a pending
/// conflict does not drag the rate (and thus inflate the ETA).
#[derive(Debug, Default)]
struct Meter {
    window_start: Option<Instant>,
    window_bytes: u64,
    window_items: u64,
    active: Duration,
    bytes_rate: f64,
    items_rate: f64,
    measured: bool,
}

impl Meter {
    fn resume(&mut self) {
        self.window_start = Some(Instant::now());
        self.window_bytes = 0;
        self.window_items = 0;
    }

    fn pause(&mut self) {
        if let Some(start) = self.window_start.take() {
            self.active += start.elapsed();
        }
    }

    fn tick(&mut self) {
        let Some(start) = self.window_start else {
            return;
        };
        let elapsed = start.elapsed();
        if elapsed < RATE_WINDOW {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let (b, i) = (
            self.window_bytes as f64 / secs,
            self.window_items as f64 / secs,
        );
        // Exponential smoothing: responsive to a real change in speed (USB stick cache
        // filling up) without jittering on every window.
        if self.measured {
            self.bytes_rate = 0.7 * self.bytes_rate + 0.3 * b;
            self.items_rate = 0.7 * self.items_rate + 0.3 * i;
        } else {
            self.bytes_rate = b;
            self.items_rate = i;
            self.measured = true;
        }
        self.active += elapsed;
        self.resume();
    }

    fn warmed_up(&self) -> bool {
        self.measured && self.active >= ETA_WARMUP
    }
}

struct Job<'a> {
    request: &'a FsOpRequest,
    sink: &'a dyn ProgressSink,
    control: &'a JobControl,
    p: Progress,
    /// The effective policy; becomes concrete once the user answers with `apply_to_all`.
    policy: ConflictPolicy,
    meter: Meter,
    last_emit: Option<Instant>,
    buf: Vec<u8>,
    verify_buf: Vec<u8>,
    succeeded: u64,
    suppressed_errors: u64,
}

impl<'a> Job<'a> {
    fn new(
        id: OpId,
        request: &'a FsOpRequest,
        sink: &'a dyn ProgressSink,
        control: &'a JobControl,
    ) -> Self {
        Self {
            request,
            sink,
            control,
            p: initial_progress(id, request.kind),
            policy: request.conflict,
            meter: Meter::default(),
            last_emit: None,
            buf: Vec::new(),
            verify_buf: Vec::new(),
            succeeded: 0,
            suppressed_errors: 0,
        }
    }

    fn run(mut self) -> Result<()> {
        // First event carries the OpId; the UI needs it to control the job.
        self.emit(true);
        let result = self.execute();

        if self.suppressed_errors > 0 {
            self.p.errors.push(OpError {
                path: String::new(),
                message: format!("…and {} more errors", self.suppressed_errors),
            });
        }
        let failed_everything = self.succeeded == 0 && !self.p.errors.is_empty();
        self.p.state = match &result {
            Err(FxError::Cancelled) => OpState::Cancelled,
            Err(_) => OpState::Failed,
            Ok(()) if failed_everything => OpState::Failed,
            Ok(()) => OpState::Done,
        };
        if let Err(e) = &result {
            if e.is_user_visible() {
                self.p.errors.push(OpError {
                    path: String::new(),
                    message: e.to_string(),
                });
            }
        }
        self.p.current.clear();
        self.p.pending_conflict = None;
        self.p.eta_secs = None;
        self.p.bytes_per_sec = 0;
        // Terminal snapshot always goes out; the UI otherwise shows a job stuck at 99%.
        self.emit(true);

        match result {
            Ok(()) if failed_everything => Err(FxError::Other(
                self.p
                    .errors
                    .first()
                    .map(|e| format!("{}: {}", e.path, e.message))
                    .unwrap_or_default(),
            )),
            other => other,
        }
    }

    fn execute(&mut self) -> Result<()> {
        let dest = match self.request.kind {
            FsOpKind::Copy | FsOpKind::Move => Some(self.prepare_destination()?),
            FsOpKind::Delete | FsOpKind::Trash => None,
        };
        let roots = self.scan(dest.as_deref())?;

        self.p.state = OpState::Running;
        self.meter.resume();
        self.emit(true);

        match (self.request.kind, dest) {
            (FsOpKind::Copy | FsOpKind::Move, Some(dest)) => {
                for root in &roots {
                    self.transfer_root(root, &dest)?;
                }
                Ok(())
            }
            (FsOpKind::Delete, _) => self.delete(&roots),
            (FsOpKind::Trash, _) => self.trash(&roots),
            // validate() guarantees copy/move carry a destination.
            (_, None) => Err(FxError::BadQuery("copy/move requires a destination".into())),
        }
    }

    fn prepare_destination(&self) -> Result<PathBuf> {
        let dest = PathBuf::from(self.request.destination.as_deref().unwrap_or_default());
        match fs::metadata(&dest) {
            Ok(m) if m.is_dir() => Ok(dest),
            Ok(_) => Err(FxError::InvalidPath(format!(
                "destination is not a folder: {}",
                dest.display()
            ))),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                fs::create_dir_all(&dest)?;
                Ok(dest)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Bytes of work a file represents: one pass to copy, a second to verify.
    fn passes(&self) -> u64 {
        if self.request.verify {
            2
        } else {
            1
        }
    }

    fn file_work(&self, node: &Node) -> u64 {
        match (self.request.kind, node.kind) {
            (_, NodeKind::Dir | NodeKind::Symlink) => 0,
            (FsOpKind::Copy | FsOpKind::Move, NodeKind::File) => node.size * self.passes(),
            (FsOpKind::Delete | FsOpKind::Trash, NodeKind::File) => node.size,
        }
    }

    // ---- phase 1: scan ------------------------------------------------------------------

    fn scan(&mut self, dest: Option<&Path>) -> Result<Vec<Root>> {
        let mut roots = Vec::with_capacity(self.request.sources.len());
        for src in &self.request.sources {
            self.checkpoint()?;
            let src = PathBuf::from(src);
            let meta = match fs::symlink_metadata(&src) {
                Ok(m) => m,
                Err(e) => {
                    self.record_error(&src, &FxError::from(e));
                    continue;
                }
            };
            let kind = NodeKind::of(&meta);
            let whole = match self.request.kind {
                FsOpKind::Trash => true,
                FsOpKind::Move => dest.is_some_and(|d| {
                    let target = src.file_name().map(|n| d.join(n));
                    target.is_some_and(|t| fs::symlink_metadata(t).is_err()) && same_volume(&src, d)
                }),
                FsOpKind::Copy | FsOpKind::Delete => false,
            };

            let root_node = Node {
                rel: PathBuf::new(),
                kind,
                size: if kind == NodeKind::File && !whole {
                    meta.len()
                } else {
                    0
                },
            };
            let mut root = Root {
                src,
                nodes: vec![root_node],
                whole,
            };
            if kind == NodeKind::Dir && !whole {
                self.walk(&root.src, &mut root.nodes)?;
            }
            self.count(&root.nodes);
            roots.push(root);
        }
        Ok(roots)
    }

    fn count(&mut self, nodes: &[Node]) {
        for n in nodes {
            self.p.total_items += 1;
            self.p.total_bytes += self.file_work(n);
        }
    }

    /// Collects everything under `root`, parents before children. Never follows symlinks
    /// or junctions: copying through one duplicates data, deleting through one destroys
    /// data outside the selection, and a loop never ends.
    fn walk(&mut self, root: &Path, nodes: &mut Vec<Node>) -> Result<()> {
        let mut pending = vec![PathBuf::new()];
        let mut seen = 0usize;
        while let Some(rel) = pending.pop() {
            let dir = under(root, &rel);
            let read = match fs::read_dir(&dir) {
                Ok(r) => r,
                Err(e) => {
                    self.record_error(&dir, &FxError::from(e));
                    continue;
                }
            };
            for item in read {
                seen += 1;
                if seen.is_multiple_of(256) {
                    self.checkpoint()?;
                    self.p.current = dir.to_string_lossy().into_owned();
                    // Running totals while scanning: "Found 12,345 items" is honest even
                    // though the final count is not yet known.
                    self.p.total_items = self.p.total_items.max(nodes.len() as u64);
                    self.emit(false);
                }
                let item = match item {
                    Ok(i) => i,
                    Err(e) => {
                        self.record_error(&dir, &FxError::from(e));
                        continue;
                    }
                };
                let meta = match item.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        self.record_error(&item.path(), &FxError::from(e));
                        continue;
                    }
                };
                let kind = NodeKind::of(&meta);
                let child = rel.join(item.file_name());
                if kind == NodeKind::Dir {
                    pending.push(child.clone());
                }
                nodes.push(Node {
                    rel: child,
                    kind,
                    size: if kind == NodeKind::File {
                        meta.len()
                    } else {
                        0
                    },
                });
            }
        }
        Ok(())
    }

    // ---- phase 2: copy / move -----------------------------------------------------------

    fn transfer_root(&mut self, root: &Root, dest: &Path) -> Result<()> {
        let Some(name) = root.src.file_name() else {
            self.record_error(
                &root.src,
                &FxError::InvalidPath("a volume root cannot be copied or moved".into()),
            );
            self.p.done_items += root.nodes.len() as u64;
            return Ok(());
        };
        let mut target_root = dest.join(name);
        let is_move = self.request.kind == FsOpKind::Move;

        if root.whole {
            return self.move_whole(root, &target_root, dest);
        }

        // Moving something onto itself is a no-op; copying it makes a sibling copy.
        if is_same_entry(&root.src, &target_root) {
            if is_move {
                self.p.done_items += root.nodes.len() as u64;
                self.p.done_bytes += root.nodes.iter().map(|n| self.file_work(n)).sum::<u64>();
                self.succeeded += 1;
                return Ok(());
            }
            target_root = disambiguate(dest, &name.to_string_lossy());
        } else if root.nodes[0].kind == NodeKind::Dir
            && self.policy == ConflictPolicy::Rename
            && fs::symlink_metadata(&target_root).is_ok()
        {
            target_root = disambiguate(dest, &name.to_string_lossy());
        }

        // Subtrees abandoned because their directory could not be created.
        let mut abandoned: Vec<&Path> = Vec::new();
        for node in &root.nodes {
            self.checkpoint()?;
            let work = self.file_work(node);
            if abandoned.iter().any(|a| node.rel.starts_with(a)) {
                self.finish_item(self.p.done_bytes, work, false);
                continue;
            }
            let src = under(&root.src, &node.rel);
            let dst = under(&target_root, &node.rel);
            self.p.current = src.to_string_lossy().into_owned();
            self.emit(false);

            let start = self.p.done_bytes;
            let outcome = match node.kind {
                NodeKind::Dir => ensure_dir(&dst),
                NodeKind::File | NodeKind::Symlink => self.transfer_file(&src, &dst, node),
            };
            match outcome {
                Ok(()) => self.finish_item(start, work, true),
                Err(FxError::Cancelled) => return Err(FxError::Cancelled),
                Err(e) => {
                    self.record_error(&src, &e);
                    if node.kind == NodeKind::Dir {
                        abandoned.push(&node.rel);
                    }
                    self.finish_item(start, work, false);
                }
            }
        }

        if is_move && root.nodes[0].kind == NodeKind::Dir {
            // Children first. A directory still holding a skipped or failed file stays,
            // which is correct: its contents were not moved.
            for node in root.nodes.iter().rev().filter(|n| n.kind == NodeKind::Dir) {
                let _ = fs::remove_dir(under(&root.src, &node.rel));
            }
        }
        Ok(())
    }

    /// Same-volume move of a whole tree: one rename. Falls back to walking and moving item
    /// by item if the rename turns out to cross a device (a mount point the volume check
    /// could not see) or the target appeared since the scan.
    fn move_whole(&mut self, root: &Root, target: &Path, dest: &Path) -> Result<()> {
        self.p.current = root.src.to_string_lossy().into_owned();
        self.emit(false);
        let start = self.p.done_bytes;
        let fallback = match fs::symlink_metadata(target) {
            Ok(_) => true,
            Err(_) => match fs::rename(&root.src, target) {
                Ok(()) => {
                    self.finish_item(start, 0, true);
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::CrossesDevices => true,
                Err(e) => {
                    self.record_error(&root.src, &FxError::from(e));
                    self.finish_item(start, 0, false);
                    return Ok(());
                }
            },
        };
        debug_assert!(fallback);

        // Re-plan this root the long way. The denominator grows, which is honest: there
        // is now more work than the scan could have known about.
        let mut nodes = vec![Node {
            rel: PathBuf::new(),
            kind: root.nodes[0].kind,
            size: fs::symlink_metadata(&root.src)
                .map(|m| m.len())
                .unwrap_or(0),
        }];
        if nodes[0].kind == NodeKind::Dir {
            nodes[0].size = 0;
            self.walk(&root.src, &mut nodes)?;
        }
        self.p.total_items -= 1;
        self.count(&nodes);
        let replanned = Root {
            src: root.src.clone(),
            nodes,
            whole: false,
        };
        self.transfer_root(&replanned, dest)
    }

    /// Copies or moves one file or symlink, applying the conflict policy.
    fn transfer_file(&mut self, src: &Path, dst: &Path, node: &Node) -> Result<()> {
        let mut dst = dst.to_path_buf();
        if let Ok(existing) = fs::symlink_metadata(&dst) {
            match self.resolve_conflict(src, &dst, &existing)? {
                Decision::Skip => return Ok(()),
                Decision::RenameTo(p) => dst = p,
                Decision::Proceed if existing.is_dir() => {
                    return Err(FxError::AlreadyExists(format!(
                        "a folder named {} is in the way",
                        dst.display()
                    )));
                }
                Decision::Proceed => {}
            }
        }

        if self.request.kind == FsOpKind::Move {
            match fs::rename(src, &dst) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::CrossesDevices => {}
                Err(e) => return Err(e.into()),
            }
        }
        match node.kind {
            NodeKind::Symlink => copy_symlink(src, &dst)?,
            _ => self.copy_file(src, &dst)?,
        }
        if self.request.kind == FsOpKind::Move {
            remove_file(src)?;
        }
        Ok(())
    }

    fn resolve_conflict(
        &mut self,
        src: &Path,
        dst: &Path,
        existing: &fs::Metadata,
    ) -> Result<Decision> {
        let src_meta = fs::symlink_metadata(src)?;
        let policy = match self.policy {
            ConflictPolicy::Ask => self.ask(src, dst, &src_meta, existing)?,
            p => p,
        };
        Ok(match policy {
            ConflictPolicy::Skip => Decision::Skip,
            ConflictPolicy::Overwrite => Decision::Proceed,
            ConflictPolicy::Rename => {
                let parent = dst.parent().unwrap_or(Path::new(""));
                let name = dst.file_name().unwrap_or_default().to_string_lossy();
                Decision::RenameTo(disambiguate(parent, &name))
            }
            ConflictPolicy::KeepNewer => {
                if mtime(&src_meta) > mtime(existing) {
                    Decision::Proceed
                } else {
                    Decision::Skip
                }
            }
            // `apply` refuses Ask as an answer, so this cannot happen; skipping is the
            // choice that cannot lose data.
            ConflictPolicy::Ask => Decision::Skip,
        })
    }

    /// Parks the job until the UI answers a conflict.
    fn ask(
        &mut self,
        src: &Path,
        dst: &Path,
        src_meta: &fs::Metadata,
        dst_meta: &fs::Metadata,
    ) -> Result<ConflictPolicy> {
        {
            let mut s = self.control.state.lock();
            s.awaiting_conflict = true;
            s.resolution = None;
        }
        self.p.pending_conflict = Some(Conflict {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            source_size: src_meta.len(),
            dest_size: dst_meta.len(),
            source_modified: mtime(src_meta),
            dest_modified: mtime(dst_meta),
        });
        let resumed_state = self.p.state;
        self.p.state = OpState::Paused;
        self.meter.pause();
        self.emit(true);

        let answer = self.park(|s| s.resolution.take());
        self.control.state.lock().awaiting_conflict = false;
        self.p.pending_conflict = None;
        let (policy, apply_to_all) = answer?;
        if apply_to_all {
            self.policy = policy;
        }
        self.p.state = resumed_state;
        self.meter.resume();
        self.emit(true);
        Ok(policy)
    }

    /// Blocks until `ready` yields a value, re-emitting the current snapshot periodically.
    /// Returns `Cancelled` if the job is cancelled meanwhile. Never holds the control lock
    /// across an emit, so a sink may call [`OpQueue::control`] from inside `emit`.
    fn park<T>(&mut self, mut ready: impl FnMut(&mut ControlState) -> Option<T>) -> Result<T> {
        loop {
            {
                let mut s = self.control.state.lock();
                if s.cancelled {
                    return Err(FxError::Cancelled);
                }
                if let Some(v) = ready(&mut s) {
                    return Ok(v);
                }
                let timed_out = self
                    .control
                    .wake
                    .wait_for(&mut s, PARKED_REEMIT)
                    .timed_out();
                if s.cancelled {
                    return Err(FxError::Cancelled);
                }
                if let Some(v) = ready(&mut s) {
                    return Ok(v);
                }
                if !timed_out {
                    continue;
                }
            }
            if !self.sink.should_continue() {
                return Err(FxError::Cancelled);
            }
            self.emit(true);
        }
    }

    /// The cooperative yield point: cancellation and pause are honoured here. Called
    /// between items and between every chunk of a copy.
    fn checkpoint(&mut self) -> Result<()> {
        if !self.sink.should_continue() {
            return Err(FxError::Cancelled);
        }
        {
            let s = self.control.state.lock();
            if s.cancelled {
                return Err(FxError::Cancelled);
            }
            if !s.paused {
                return Ok(());
            }
        }
        let resumed_state = self.p.state;
        self.p.state = OpState::Paused;
        self.meter.pause();
        self.emit(true);
        self.park(|s| (!s.paused).then_some(()))?;
        self.p.state = resumed_state;
        self.meter.resume();
        self.emit(true);
        Ok(())
    }

    /// Streams `src` into a partial file, verifies it if asked, then renames it into
    /// place.
    fn copy_file(&mut self, src: &Path, dst: &Path) -> Result<()> {
        let part = partial_path(dst);
        let result = self.copy_into(src, &part).and_then(|perms| {
            if self.request.verify {
                self.verify(src, &part)?;
            }
            fs::rename(&part, dst)?;
            // Permissions last: a read-only file could not have been renamed over on
            // Windows, nor verified-then-replaced.
            fs::set_permissions(dst, perms)?;
            Ok(())
        });
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result
    }

    fn copy_into(&mut self, src: &Path, part: &Path) -> Result<fs::Permissions> {
        let mut reader = File::open(src)?;
        let meta = reader.metadata()?;
        let mut writer = File::create(part)?;
        let mut buf = std::mem::take(&mut self.buf);
        buf.resize(COPY_CHUNK, 0);
        let result = (|| -> Result<()> {
            loop {
                self.checkpoint()?;
                let n = match reader.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                writer.write_all(&buf[..n])?;
                self.add_bytes(n as u64);
            }
        })();
        self.buf = buf;
        result?;
        // Explorer keeps the modified time on copy; so do we. Set after the last write,
        // which would otherwise bump it.
        if let Ok(modified) = meta.modified() {
            let _ = writer.set_modified(modified);
        }
        Ok(meta.permissions())
    }

    fn verify(&mut self, src: &Path, part: &Path) -> Result<()> {
        let mut a = File::open(src)?;
        let mut b = File::open(part)?;
        let mut buf_a = std::mem::take(&mut self.buf);
        let mut buf_b = std::mem::take(&mut self.verify_buf);
        buf_a.resize(COPY_CHUNK, 0);
        buf_b.resize(COPY_CHUNK, 0);
        let result = streams_equal(&mut a, &mut b, &mut buf_a, &mut buf_b, |n| {
            self.checkpoint()?;
            self.add_bytes(n);
            Ok(())
        });
        self.buf = buf_a;
        self.verify_buf = buf_b;
        if result? {
            Ok(())
        } else {
            Err(FxError::Io(
                "verification failed: the copy does not match the source".into(),
            ))
        }
    }

    // ---- phase 2: delete / trash --------------------------------------------------------

    fn delete(&mut self, roots: &[Root]) -> Result<()> {
        for root in roots {
            // Reverse of parents-before-children is children-before-parents.
            for node in root.nodes.iter().rev() {
                self.checkpoint()?;
                let path = under(&root.src, &node.rel);
                self.p.current = path.to_string_lossy().into_owned();
                self.emit(false);
                let start = self.p.done_bytes;
                let work = self.file_work(node);
                let result = match node.kind {
                    NodeKind::Dir => fs::remove_dir(&path).map_err(FxError::from),
                    NodeKind::File | NodeKind::Symlink => remove_file(&path),
                };
                match result {
                    Ok(()) => self.finish_item(start, work, true),
                    Err(e) => {
                        self.record_error(&path, &e);
                        self.finish_item(start, work, false);
                    }
                }
            }
        }
        Ok(())
    }

    fn trash(&mut self, roots: &[Root]) -> Result<()> {
        for root in roots {
            self.checkpoint()?;
            self.p.current = root.src.to_string_lossy().into_owned();
            self.emit(false);
            let start = self.p.done_bytes;
            match crate::trash_path(&root.src) {
                Ok(()) => self.finish_item(start, 0, true),
                Err(e) => {
                    self.record_error(&root.src, &e);
                    self.finish_item(start, 0, false);
                }
            }
        }
        Ok(())
    }

    // ---- accounting ---------------------------------------------------------------------

    fn add_bytes(&mut self, n: u64) {
        self.meter.window_bytes += n;
        // A file that grew since the scan must not push the bar past 100%.
        self.p.done_bytes = (self.p.done_bytes + n).min(self.p.total_bytes);
        self.emit(false);
    }

    /// Closes out one item. Its bytes count as processed whether it was copied, skipped or
    /// failed, so the bar always reaches the end.
    fn finish_item(&mut self, start_bytes: u64, work: u64, ok: bool) {
        self.p.done_bytes = (start_bytes + work).min(self.p.total_bytes);
        self.p.done_items += 1;
        self.meter.window_items += 1;
        if ok {
            self.succeeded += 1;
        }
    }

    fn record_error(&mut self, path: &Path, error: &FxError) {
        if self.p.errors.len() >= MAX_ERRORS {
            self.suppressed_errors += 1;
            return;
        }
        self.p.errors.push(OpError {
            path: path.to_string_lossy().into_owned(),
            message: error.to_string(),
        });
    }

    fn emit(&mut self, force: bool) {
        let now = Instant::now();
        if !force && self.last_emit.is_some_and(|t| now - t < EMIT_INTERVAL) {
            return;
        }
        self.last_emit = Some(now);
        self.refresh_estimates();
        self.sink.emit(&self.p);
    }

    fn refresh_estimates(&mut self) {
        if self.p.state != OpState::Running {
            self.p.bytes_per_sec = 0;
            self.p.eta_secs = None;
            return;
        }
        self.meter.tick();
        self.p.bytes_per_sec = self.meter.bytes_rate as u64;
        if !self.meter.warmed_up() {
            self.p.eta_secs = None;
            return;
        }
        // Deletion time is per item, not per byte: deleting a 4 GB file is as quick as a
        // 4 KB one. Moves made of renames have no bytes to measure either.
        let by_items = matches!(self.request.kind, FsOpKind::Delete | FsOpKind::Trash)
            || self.p.total_bytes == 0;
        self.p.eta_secs = if by_items {
            let remaining = self.p.total_items.saturating_sub(self.p.done_items);
            (self.meter.items_rate > 0.0)
                .then(|| (remaining as f64 / self.meter.items_rate).ceil() as u64)
        } else {
            estimate_eta(self.p.done_bytes, self.p.total_bytes, self.p.bytes_per_sec)
        };
    }
}

// ---- free helpers -----------------------------------------------------------------------

/// Compares two streams chunk by chunk, calling `tick` with the bytes consumed from each
/// so verification shows up as progress and can be cancelled.
fn streams_equal(
    a: &mut impl Read,
    b: &mut impl Read,
    buf_a: &mut [u8],
    buf_b: &mut [u8],
    mut tick: impl FnMut(u64) -> Result<()>,
) -> Result<bool> {
    loop {
        let n = read_full(a, buf_a)?;
        let m = read_full(b, buf_b)?;
        if n != m || buf_a[..n] != buf_b[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        tick(n as u64)?;
    }
}

/// Reads until `buf` is full or the stream ends, so two streams with different read
/// granularity still compare chunk-aligned.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(filled)
}

fn partial_path(dst: &Path) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    dst.with_file_name(format!(".{name}{PARTIAL_SUFFIX}"))
}

fn ensure_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Merging into an existing folder is the expected case, not a conflict.
        Err(e) if e.kind() == ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(FxError::AlreadyExists(format!(
            "a file named {} is in the way",
            path.display()
        ))),
        Err(e) => Err(e.into()),
    }
}

/// Deletes a file or symlink. Clears the read-only attribute and retries, because on
/// Windows read-only files refuse deletion — and a directory symlink or junction needs
/// `remove_dir`, which removes the link and never what it points to.
fn remove_file(path: &Path) -> Result<()> {
    let first = match fs::remove_file(path) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    if fs::remove_dir(path).is_ok() {
        return Ok(());
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() && !meta.file_type().is_symlink() {
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            if fs::set_permissions(path, perms).is_ok() && fs::remove_file(path).is_ok() {
                return Ok(());
            }
        }
    }
    Err(first.into())
}

fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src)?;
    if fs::symlink_metadata(dst).is_ok() {
        remove_file(dst)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, dst)?;
    #[cfg(windows)]
    {
        // Creating symlinks needs Developer Mode or the privilege; without it this fails
        // per item and is reported, rather than silently copying the target's contents.
        let resolved = src
            .parent()
            .map(|p| p.join(&target))
            .unwrap_or(target.clone());
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(&target, dst)?;
        } else {
            std::os::windows::fs::symlink_file(&target, dst)?;
        }
    }
    Ok(())
}

fn mtime(meta: &fs::Metadata) -> i64 {
    crate::list::system_time_millis(meta.modified().ok())
}

/// Whether two paths name the same filesystem object (also true for a case-only
/// difference on a case-insensitive volume).
fn is_same_entry(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y || same_file_id(a, b),
        _ => false,
    }
}

#[cfg(unix)]
fn same_file_id(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file_id(_a: &Path, _b: &Path) -> bool {
    // `canonicalize` on Windows returns the on-disk casing, so the path comparison above
    // already covers case-only differences.
    false
}

#[cfg(unix)]
fn same_volume(src: &Path, dest_dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(src), fs::metadata(dest_dir)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Compares drive prefixes. Mount points inside a volume can fool this, which is why
/// [`Job::move_whole`] still falls back when the rename reports a cross-device move.
#[cfg(not(unix))]
fn same_volume(src: &Path, dest_dir: &Path) -> bool {
    use std::path::Component;
    let prefix = |p: &Path| match p.components().next() {
        Some(Component::Prefix(px)) => Some(px.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    };
    match (prefix(src), prefix(dest_dir)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn req(kind: FsOpKind, sources: &[&str], dest: Option<&str>) -> FsOpRequest {
        FsOpRequest {
            kind,
            sources: sources.iter().map(|s| s.to_string()).collect(),
            destination: dest.map(String::from),
            conflict: ConflictPolicy::Ask,
            verify: false,
        }
    }

    fn s(p: &Path) -> &str {
        p.to_str().unwrap()
    }

    type Hook = Box<dyn Fn(&Progress) + Send + Sync>;

    /// Records every snapshot; optionally reacts to them or cancels after N checks.
    #[derive(Default)]
    struct Rec {
        events: Mutex<Vec<Progress>>,
        hook: Option<Hook>,
        stop_after_checks: Option<usize>,
        checks: AtomicUsize,
    }

    impl Rec {
        fn with_hook(hook: impl Fn(&Progress) + Send + Sync + 'static) -> Self {
            Self {
                hook: Some(Box::new(hook)),
                ..Default::default()
            }
        }
        fn last(&self) -> Progress {
            self.events.lock().last().cloned().unwrap()
        }
        fn states(&self) -> Vec<OpState> {
            self.events.lock().iter().map(|p| p.state).collect()
        }
        fn conflicts_asked(&self) -> usize {
            // Consecutive re-emissions of one parked conflict count once.
            let ev = self.events.lock();
            let mut n = 0;
            let mut prev: Option<&Conflict> = None;
            for p in ev.iter() {
                if let Some(c) = &p.pending_conflict {
                    if prev != Some(c) {
                        n += 1;
                    }
                }
                prev = p.pending_conflict.as_ref();
            }
            n
        }
    }

    impl ProgressSink for Rec {
        fn emit(&self, p: &Progress) {
            self.events.lock().push(p.clone());
            if let Some(h) = &self.hook {
                h(p);
            }
        }
        fn should_continue(&self) -> bool {
            let n = self.checks.fetch_add(1, Ordering::Relaxed);
            self.stop_after_checks.is_none_or(|limit| n < limit)
        }
    }

    /// A queue that can be shared with a sink hook (which needs `'static`).
    fn shared_queue() -> Arc<OpQueue> {
        Arc::new(OpQueue::new())
    }

    fn resolver(q: &Arc<OpQueue>, policy: ConflictPolicy, apply_to_all: bool) -> Rec {
        let q = Arc::clone(q);
        Rec::with_hook(move |p| {
            if p.pending_conflict.is_some() {
                let _ = q.control(
                    p.op,
                    OpControl::Resolve {
                        policy,
                        apply_to_all,
                    },
                );
            }
        })
    }

    /// src/{a.txt,b.txt,sub/c.txt} and dst/ holding stale copies of a.txt and b.txt.
    struct Fx {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        dst: PathBuf,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("a.txt"), b"new a").unwrap();
        fs::write(src.join("b.txt"), b"new b").unwrap();
        fs::write(src.join("sub/c.txt"), b"new c").unwrap();
        fs::write(dst.join("a.txt"), b"old a").unwrap();
        fs::write(dst.join("b.txt"), b"old b").unwrap();
        Fx {
            _tmp: tmp,
            src,
            dst,
        }
    }

    impl Fx {
        fn files(&self) -> Vec<String> {
            vec![
                s(&self.src.join("a.txt")).to_string(),
                s(&self.src.join("b.txt")).to_string(),
            ]
        }
        fn copy(&self, policy: ConflictPolicy) -> FsOpRequest {
            FsOpRequest {
                kind: FsOpKind::Copy,
                sources: self.files(),
                destination: Some(s(&self.dst).to_string()),
                conflict: policy,
                verify: false,
            }
        }
        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.dst.join(name)).unwrap()
        }
    }

    fn set_mtime(p: &Path, secs_ago: u64) {
        let t = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
        File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn no_partials(dir: &Path) -> bool {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(PARTIAL_SUFFIX))
    }

    // ---- validation and naming -------------------------------------------------

    #[test]
    fn rejects_empty_source_list() {
        let e = validate(&req(FsOpKind::Copy, &[], Some("/tmp"))).unwrap_err();
        assert!(matches!(e, FxError::BadQuery(_)));
    }

    #[test]
    fn copy_without_destination_is_rejected() {
        let e = validate(&req(FsOpKind::Copy, &["/tmp/a"], None)).unwrap_err();
        assert!(matches!(e, FxError::BadQuery(_)));
    }

    #[test]
    fn delete_needs_no_destination() {
        assert!(validate(&req(FsOpKind::Delete, &["/tmp/a"], None)).is_ok());
        assert!(validate(&req(FsOpKind::Trash, &["/tmp/a"], None)).is_ok());
    }

    #[test]
    fn refuses_to_move_a_directory_into_itself() {
        let e = validate(&req(FsOpKind::Move, &["/tmp/src"], Some("/tmp/src/inner"))).unwrap_err();
        assert!(matches!(e, FxError::InvalidPath(_)), "got {e:?}");

        // Same check for Windows-style separators.
        let e = validate(&req(FsOpKind::Move, &[r"C:\data"], Some(r"C:\data\sub"))).unwrap_err();
        assert!(matches!(e, FxError::InvalidPath(_)), "got {e:?}");

        // A sibling directory with a shared prefix is legitimate and must be allowed.
        assert!(validate(&req(FsOpKind::Move, &["/tmp/src"], Some("/tmp/src2"))).is_ok());
    }

    #[test]
    fn rename_rejects_path_separators_and_collisions() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.txt");
        fs::write(&a, b"x").unwrap();
        fs::write(d.path().join("b.txt"), b"y").unwrap();

        assert!(matches!(
            rename(s(&a), "sub/c.txt"),
            Err(FxError::InvalidPath(_))
        ));
        assert!(matches!(rename(s(&a), ""), Err(FxError::InvalidPath(_))));
        assert!(matches!(
            rename(s(&a), "b.txt"),
            Err(FxError::AlreadyExists(_))
        ));

        rename(s(&a), "renamed.txt").unwrap();
        assert!(d.path().join("renamed.txt").exists());
    }

    #[test]
    fn rename_allows_a_case_only_change() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("readme");
        fs::write(&a, b"x").unwrap();
        rename(s(&a), "README").unwrap();
        let names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["README"]);
    }

    #[test]
    fn disambiguate_follows_explorer_naming() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("report.pdf"), b"x").unwrap();
        assert_eq!(
            disambiguate(d.path(), "report.pdf").file_name().unwrap(),
            "report (2).pdf"
        );

        fs::write(d.path().join("report (2).pdf"), b"x").unwrap();
        assert_eq!(
            disambiguate(d.path(), "report.pdf").file_name().unwrap(),
            "report (3).pdf"
        );

        // Extensionless files keep the suffix outside any dot.
        fs::write(d.path().join("Makefile"), b"x").unwrap();
        assert_eq!(
            disambiguate(d.path(), "Makefile").file_name().unwrap(),
            "Makefile (2)"
        );

        // Multi-dot names number the last stem, like Explorer: "a.tar (2).gz".
        fs::write(d.path().join("a.tar.gz"), b"x").unwrap();
        assert_eq!(
            disambiguate(d.path(), "a.tar.gz").file_name().unwrap(),
            "a.tar (2).gz"
        );
    }

    #[test]
    fn eta_is_absent_until_it_would_be_honest() {
        // No throughput measured yet.
        assert_eq!(estimate_eta(0, 1000, 0), None);
        // Total unknown because the scan has not finished.
        assert_eq!(estimate_eta(0, 0, 100), None);
        // Nonsensical input must not produce a wildly wrong number.
        assert_eq!(estimate_eta(2000, 1000, 100), None);

        assert_eq!(estimate_eta(500, 1000, 100), Some(5));
    }

    #[test]
    fn initial_progress_starts_scanning_without_an_estimate() {
        let p = initial_progress(OpId(1), FsOpKind::Copy);
        assert_eq!(p.state, OpState::Scanning);
        assert_eq!(p.eta_secs, None);
        assert!(p.errors.is_empty());
    }

    #[test]
    fn op_ids_are_unique_and_nonzero() {
        let q = OpQueue::new();
        let a = q.allocate();
        let b = q.allocate();
        assert_ne!(a, b);
        assert_ne!(a.0, 0, "zero is reserved as a sentinel");
    }

    // ---- copy -----------------------------------------------------------------------------

    #[test]
    fn copy_scans_first_then_runs_to_done() {
        let f = fx();
        let q = OpQueue::new();
        let sink = Rec::default();
        let request = FsOpRequest {
            sources: vec![s(&f.src).to_string()],
            ..f.copy(ConflictPolicy::Skip)
        };
        let id = q.start(request, &sink).unwrap();

        let states = sink.states();
        assert_eq!(states.first(), Some(&OpState::Scanning));
        assert_eq!(states.last(), Some(&OpState::Done));
        let first_running = states.iter().position(|s| *s == OpState::Running).unwrap();
        assert!(states[..first_running]
            .iter()
            .all(|s| *s == OpState::Scanning));

        // Every snapshot carries the id; the UI learns it from the first one.
        assert!(sink.events.lock().iter().all(|p| p.op == id));
        // The denominator is fixed from the first Running snapshot onwards.
        let running = sink.events.lock()[first_running].clone();
        let last = sink.last();
        assert_eq!(running.total_items, last.total_items);
        assert_eq!(last.total_items, 5, "src, a, b, sub, sub/c");
        assert_eq!(last.total_bytes, 15);
        assert_eq!(last.done_bytes, last.total_bytes);
        assert_eq!(last.done_items, last.total_items);
        // ETA is never present during scanning.
        assert!(sink
            .events
            .lock()
            .iter()
            .filter(|p| p.state == OpState::Scanning)
            .all(|p| p.eta_secs.is_none()));

        assert_eq!(
            fs::read_to_string(f.dst.join("src/sub/c.txt")).unwrap(),
            "new c"
        );
        assert_eq!(q.active(), 0, "finished jobs leave the table");
    }

    #[test]
    fn copy_preserves_modified_time_and_cleans_up_partials() {
        let f = fx();
        set_mtime(&f.src.join("a.txt"), 86_400);
        let request = FsOpRequest {
            sources: vec![s(&f.src.join("a.txt")).to_string()],
            ..f.copy(ConflictPolicy::Overwrite)
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        let m = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(m(&f.src.join("a.txt")), m(&f.dst.join("a.txt")));
        assert!(no_partials(&f.dst));
    }

    #[test]
    fn copy_large_file_in_chunks_with_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.bin");
        let data: Vec<u8> = (0..COPY_CHUNK * 3 + 123).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &data).unwrap();
        let dst = tmp.path().join("out");

        let sink = Rec::default();
        let request = FsOpRequest {
            kind: FsOpKind::Copy,
            sources: vec![s(&src).to_string()],
            destination: Some(s(&dst).to_string()),
            conflict: ConflictPolicy::Ask,
            verify: true,
        };
        OpQueue::new().start(request, &sink).unwrap();
        assert_eq!(fs::read(dst.join("big.bin")).unwrap(), data);
        // The verification pass is part of the honest total.
        let last = sink.last();
        assert_eq!(last.total_bytes, 2 * data.len() as u64);
        assert_eq!(last.done_bytes, last.total_bytes);
        assert!(last.errors.is_empty());
        // Missing destination directories are created.
        assert!(dst.is_dir());
    }

    #[test]
    fn stream_comparison_detects_differences() {
        let mut ba = vec![0u8; 4];
        let mut bb = vec![0u8; 4];
        let same = |a: &[u8], b: &[u8], ba: &mut Vec<u8>, bb: &mut Vec<u8>| {
            streams_equal(
                &mut std::io::Cursor::new(a.to_vec()),
                &mut std::io::Cursor::new(b.to_vec()),
                ba,
                bb,
                |_| Ok(()),
            )
            .unwrap()
        };
        assert!(same(b"hello world", b"hello world", &mut ba, &mut bb));
        assert!(!same(b"hello world", b"hello wOrld", &mut ba, &mut bb));
        assert!(!same(b"hello", b"hello!", &mut ba, &mut bb));
        assert!(same(b"", b"", &mut ba, &mut bb));
    }

    #[test]
    fn copying_into_the_same_folder_makes_a_numbered_copy() {
        let f = fx();
        let request = FsOpRequest {
            sources: vec![s(&f.dst.join("a.txt")).to_string()],
            // Overwrite must never mean "truncate the file onto itself".
            ..f.copy(ConflictPolicy::Overwrite)
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        assert_eq!(f.read("a.txt"), "old a");
        assert_eq!(f.read("a (2).txt"), "old a");
    }

    #[test]
    fn existing_folders_are_merged() {
        let f = fx();
        fs::create_dir_all(f.dst.join("src")).unwrap();
        fs::write(f.dst.join("src/keep.txt"), b"keep").unwrap();
        let request = FsOpRequest {
            sources: vec![s(&f.src).to_string()],
            ..f.copy(ConflictPolicy::Skip)
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        assert_eq!(f.read("src/keep.txt"), "keep");
        assert_eq!(f.read("src/sub/c.txt"), "new c");
    }

    #[test]
    fn rename_policy_keeps_both_folders() {
        let f = fx();
        fs::create_dir_all(f.dst.join("src")).unwrap();
        let request = FsOpRequest {
            sources: vec![s(&f.src).to_string()],
            ..f.copy(ConflictPolicy::Rename)
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        assert!(f.dst.join("src (2)/sub/c.txt").exists());
        assert_eq!(fs::read_dir(f.dst.join("src")).unwrap().count(), 0);
    }

    // ---- every conflict policy -----------------------------------------------------------

    #[test]
    fn policy_skip_leaves_existing_files() {
        let f = fx();
        let sink = Rec::default();
        OpQueue::new()
            .start(f.copy(ConflictPolicy::Skip), &sink)
            .unwrap();
        assert_eq!(f.read("a.txt"), "old a");
        assert_eq!(f.read("b.txt"), "old b");
        // Skipped items still complete the bar.
        let last = sink.last();
        assert_eq!(last.state, OpState::Done);
        assert_eq!(last.done_items, last.total_items);
        assert_eq!(last.done_bytes, last.total_bytes);
    }

    #[test]
    fn policy_overwrite_replaces_files() {
        let f = fx();
        OpQueue::new()
            .start(f.copy(ConflictPolicy::Overwrite), &Rec::default())
            .unwrap();
        assert_eq!(f.read("a.txt"), "new a");
        assert_eq!(f.read("b.txt"), "new b");
        assert!(no_partials(&f.dst));
    }

    #[test]
    fn policy_rename_keeps_both_files() {
        let f = fx();
        OpQueue::new()
            .start(f.copy(ConflictPolicy::Rename), &Rec::default())
            .unwrap();
        assert_eq!(f.read("a.txt"), "old a");
        assert_eq!(f.read("a (2).txt"), "new a");
        assert_eq!(f.read("b (2).txt"), "new b");
    }

    #[test]
    fn policy_keep_newer_compares_modified_times() {
        let f = fx();
        // a: source newer -> replaced. b: destination newer -> kept.
        set_mtime(&f.dst.join("a.txt"), 3_600);
        set_mtime(&f.src.join("b.txt"), 3_600);
        OpQueue::new()
            .start(f.copy(ConflictPolicy::KeepNewer), &Rec::default())
            .unwrap();
        assert_eq!(f.read("a.txt"), "new a");
        assert_eq!(f.read("b.txt"), "old b");
    }

    #[test]
    fn policy_ask_with_apply_to_all_asks_once() {
        let f = fx();
        let q = shared_queue();
        let sink = resolver(&q, ConflictPolicy::Overwrite, true);
        q.start(f.copy(ConflictPolicy::Ask), &sink).unwrap();
        assert_eq!(sink.conflicts_asked(), 1);
        assert_eq!(f.read("a.txt"), "new a");
        assert_eq!(f.read("b.txt"), "new b");
    }

    #[test]
    fn policy_ask_without_apply_to_all_asks_per_file() {
        let f = fx();
        let q = shared_queue();
        let sink = resolver(&q, ConflictPolicy::Rename, false);
        q.start(f.copy(ConflictPolicy::Ask), &sink).unwrap();
        assert_eq!(sink.conflicts_asked(), 2);
        assert_eq!(f.read("a (2).txt"), "new a");
        assert_eq!(f.read("b (2).txt"), "new b");
    }

    #[test]
    fn ask_parks_the_job_paused_with_the_conflict_described() {
        let f = fx();
        set_mtime(&f.dst.join("a.txt"), 60);
        let q = OpQueue::new();
        let sink = Rec::default();
        std::thread::scope(|scope| {
            let job = scope.spawn(|| q.start(f.copy(ConflictPolicy::Ask), &sink));
            let parked = wait_for(&sink, |p| p.pending_conflict.is_some());
            assert_eq!(parked.state, OpState::Paused);
            assert_eq!(parked.eta_secs, None);
            let c = parked.pending_conflict.clone().unwrap();
            assert!(c.source.ends_with("a.txt") && c.destination.ends_with("a.txt"));
            assert_eq!((c.source_size, c.dest_size), (5, 5));
            assert!(c.source_modified > c.dest_modified);

            // Stays parked, and keeps re-announcing itself for throttled sinks.
            let before = sink.events.lock().len();
            std::thread::sleep(PARKED_REEMIT * 3);
            assert!(sink.events.lock().len() > before);
            assert_eq!(f.read("a.txt"), "old a");

            // Ask is not an answer.
            let ask = OpControl::Resolve {
                policy: ConflictPolicy::Ask,
                apply_to_all: true,
            };
            assert!(matches!(
                q.control(parked.op, ask),
                Err(FxError::BadQuery(_))
            ));

            let skip_all = OpControl::Resolve {
                policy: ConflictPolicy::Skip,
                apply_to_all: true,
            };
            q.control(parked.op, skip_all).unwrap();
            job.join().unwrap().unwrap();
        });
        assert_eq!(sink.last().state, OpState::Done);
        assert_eq!(f.read("a.txt"), "old a");
    }

    fn wait_for(sink: &Rec, pred: impl Fn(&Progress) -> bool) -> Progress {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(p) = sink.events.lock().iter().rev().find(|p| pred(p)) {
                return p.clone();
            }
            assert!(Instant::now() < deadline, "timed out waiting for progress");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // ---- control ------------------------------------------------------------------------

    #[test]
    fn cancel_is_honoured_between_chunks_of_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.bin");
        fs::write(&src, vec![7u8; COPY_CHUNK * 8]).unwrap();
        let dst = tmp.path().join("out");
        fs::create_dir(&dst).unwrap();

        // One file, so a cancel checked only between files would never be seen.
        let sink = Rec {
            stop_after_checks: Some(3),
            ..Default::default()
        };
        let request = FsOpRequest {
            kind: FsOpKind::Copy,
            sources: vec![s(&src).to_string()],
            destination: Some(s(&dst).to_string()),
            conflict: ConflictPolicy::Ask,
            verify: false,
        };
        let err = OpQueue::new().start(request, &sink).unwrap_err();
        assert!(matches!(err, FxError::Cancelled));

        let last = sink.last();
        assert_eq!(last.state, OpState::Cancelled);
        assert!(last.done_bytes < last.total_bytes, "stopped mid-file");
        assert!(last.errors.is_empty(), "cancelling is not an error");
        // No truncated file under the real name, and no debris.
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 0);
    }

    #[test]
    fn cancel_via_control_reaches_a_running_job() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.bin");
        fs::write(&src, vec![1u8; COPY_CHUNK * 4]).unwrap();
        let q = shared_queue();
        // Cancel from inside the first Running snapshot, i.e. before any bytes move.
        let sink = {
            let q = Arc::clone(&q);
            Rec::with_hook(move |p| {
                if p.state == OpState::Running {
                    let _ = q.control(p.op, OpControl::Cancel);
                }
            })
        };
        let request = FsOpRequest {
            kind: FsOpKind::Copy,
            sources: vec![s(&src).to_string()],
            destination: Some(s(&tmp.path().join("out")).to_string()),
            conflict: ConflictPolicy::Ask,
            verify: false,
        };
        assert!(matches!(q.start(request, &sink), Err(FxError::Cancelled)));
        assert_eq!(sink.last().done_bytes, 0);
        assert_eq!(q.active(), 0);
    }

    #[test]
    fn pause_holds_the_job_until_resumed() {
        let f = fx();
        let q = shared_queue();
        let paused_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sink = {
            let (q, paused_once) = (Arc::clone(&q), Arc::clone(&paused_once));
            Rec::with_hook(move |p| {
                if p.state == OpState::Running && !paused_once.swap(true, Ordering::SeqCst) {
                    q.control(p.op, OpControl::Pause).unwrap();
                }
            })
        };
        std::thread::scope(|scope| {
            let job = scope.spawn(|| q.start(f.copy(ConflictPolicy::Overwrite), &sink));
            let parked = wait_for(&sink, |p| p.state == OpState::Paused);
            std::thread::sleep(PARKED_REEMIT * 2);
            assert_eq!(f.read("a.txt"), "old a", "no progress while paused");
            assert_eq!(parked.bytes_per_sec, 0);
            q.control(parked.op, OpControl::Resume).unwrap();
            job.join().unwrap().unwrap();
        });
        assert_eq!(f.read("a.txt"), "new a");
        assert_eq!(sink.last().state, OpState::Done);
    }

    #[test]
    fn control_of_unknown_or_unparked_ops_is_rejected() {
        let q = shared_queue();
        assert!(matches!(
            q.control(OpId(999), OpControl::Cancel),
            Err(FxError::NotFound(_))
        ));
        // Resolve with nothing pending is a caller bug, reported as such.
        let f = fx();
        let result = Arc::new(Mutex::new(None));
        let sink = {
            let (q, result) = (Arc::clone(&q), Arc::clone(&result));
            Rec::with_hook(move |p| {
                if p.state == OpState::Running && result.lock().is_none() {
                    *result.lock() = Some(q.control(
                        p.op,
                        OpControl::Resolve {
                            policy: ConflictPolicy::Skip,
                            apply_to_all: false,
                        },
                    ));
                }
            })
        };
        q.start(f.copy(ConflictPolicy::Skip), &sink).unwrap();
        assert!(matches!(
            result.lock().clone(),
            Some(Err(FxError::BadQuery(_)))
        ));
    }

    // ---- errors -------------------------------------------------------------------------

    #[test]
    fn per_file_failures_accumulate_and_the_job_continues() {
        let f = fx();
        let mut request = f.copy(ConflictPolicy::Overwrite);
        request
            .sources
            .insert(0, s(&f.src.join("missing.txt")).to_string());
        let sink = Rec::default();
        OpQueue::new().start(request, &sink).unwrap();
        let last = sink.last();
        assert_eq!(last.state, OpState::Done);
        assert_eq!(last.errors.len(), 1);
        assert!(last.errors[0].path.ends_with("missing.txt"));
        assert_eq!(f.read("b.txt"), "new b");
    }

    #[test]
    fn total_failure_is_reported_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Rec::default();
        let request = req(
            FsOpKind::Delete,
            &[s(&tmp.path().join("nope")), s(&tmp.path().join("nada"))],
            None,
        );
        assert!(OpQueue::new().start(request, &sink).is_err());
        assert_eq!(sink.last().state, OpState::Failed);
        assert_eq!(sink.last().errors.len(), 2);
    }

    #[test]
    fn destination_that_is_a_file_fails_the_whole_op() {
        let f = fx();
        let mut request = f.copy(ConflictPolicy::Skip);
        request.destination = Some(s(&f.dst.join("a.txt")).to_string());
        let sink = Rec::default();
        let err = OpQueue::new().start(request, &sink).unwrap_err();
        assert!(matches!(err, FxError::InvalidPath(_)));
        assert_eq!(sink.last().state, OpState::Failed);
    }

    #[test]
    fn a_file_cannot_overwrite_a_folder() {
        let f = fx();
        fs::remove_file(f.dst.join("a.txt")).unwrap();
        fs::create_dir(f.dst.join("a.txt")).unwrap();
        let sink = Rec::default();
        OpQueue::new()
            .start(f.copy(ConflictPolicy::Overwrite), &sink)
            .unwrap();
        assert!(f.dst.join("a.txt").is_dir());
        assert_eq!(sink.last().errors.len(), 1);
        assert_eq!(f.read("b.txt"), "new b");
    }

    #[test]
    fn errors_beyond_the_cap_are_summarised() {
        let tmp = tempfile::tempdir().unwrap();
        let missing: Vec<String> = (0..MAX_ERRORS + 10)
            .map(|i| s(&tmp.path().join(format!("m{i}"))).to_string())
            .collect();
        let sink = Rec::default();
        let request = FsOpRequest {
            kind: FsOpKind::Delete,
            sources: missing,
            destination: None,
            conflict: ConflictPolicy::Skip,
            verify: false,
        };
        let _ = OpQueue::new().start(request, &sink);
        let errors = sink.last().errors;
        assert_eq!(errors.len(), MAX_ERRORS + 1);
        assert!(errors.last().unwrap().message.contains("10 more"));
    }

    // ---- move ---------------------------------------------------------------------------

    #[test]
    fn same_volume_move_is_a_single_rename() {
        let f = fx();
        let sink = Rec::default();
        let request = FsOpRequest {
            kind: FsOpKind::Move,
            sources: vec![s(&f.src.join("sub")).to_string()],
            destination: Some(s(&f.dst).to_string()),
            conflict: ConflictPolicy::Ask,
            verify: false,
        };
        OpQueue::new().start(request, &sink).unwrap();
        assert!(!f.src.join("sub").exists());
        assert_eq!(f.read("sub/c.txt"), "new c");
        // Not walked: one item, no bytes to pretend to transfer.
        let last = sink.last();
        assert_eq!((last.total_items, last.total_bytes), (1, 0));
    }

    #[test]
    fn move_into_an_existing_folder_merges_and_removes_the_source() {
        let f = fx();
        fs::create_dir_all(f.dst.join("src")).unwrap();
        fs::write(f.dst.join("src/a.txt"), b"old").unwrap();
        let request = FsOpRequest {
            kind: FsOpKind::Move,
            sources: vec![s(&f.src).to_string()],
            destination: Some(s(&f.dst).to_string()),
            conflict: ConflictPolicy::Overwrite,
            verify: false,
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        assert!(!f.src.exists(), "emptied source tree is removed");
        assert_eq!(f.read("src/a.txt"), "new a");
        assert_eq!(f.read("src/sub/c.txt"), "new c");
    }

    #[test]
    fn move_leaves_skipped_files_and_their_folders_behind() {
        let f = fx();
        fs::create_dir_all(f.dst.join("src")).unwrap();
        fs::write(f.dst.join("src/a.txt"), b"old").unwrap();
        let request = FsOpRequest {
            kind: FsOpKind::Move,
            sources: vec![s(&f.src).to_string()],
            destination: Some(s(&f.dst).to_string()),
            conflict: ConflictPolicy::Skip,
            verify: false,
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        assert_eq!(fs::read_to_string(f.src.join("a.txt")).unwrap(), "new a");
        assert!(!f.src.join("b.txt").exists());
        assert!(!f.src.join("sub").exists());
    }

    #[test]
    fn moving_onto_itself_is_a_no_op() {
        let f = fx();
        let request = FsOpRequest {
            kind: FsOpKind::Move,
            sources: vec![s(&f.dst.join("a.txt")).to_string()],
            destination: Some(s(&f.dst).to_string()),
            conflict: ConflictPolicy::Overwrite,
            verify: false,
        };
        let sink = Rec::default();
        OpQueue::new().start(request, &sink).unwrap();
        assert_eq!(f.read("a.txt"), "old a");
        assert_eq!(sink.last().state, OpState::Done);
    }

    // ---- delete -------------------------------------------------------------------------

    #[test]
    fn delete_removes_trees_children_first() {
        let f = fx();
        let ro = f.src.join("sub/c.txt");
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro, perms).unwrap();

        let sink = Rec::default();
        let request = req(
            FsOpKind::Delete,
            &[s(&f.src), s(&f.dst.join("a.txt"))],
            None,
        );
        OpQueue::new().start(request, &sink).unwrap();
        assert!(!f.src.exists());
        assert!(!f.dst.join("a.txt").exists());
        let last = sink.last();
        assert_eq!(last.state, OpState::Done);
        assert!(last.errors.is_empty(), "{:?}", last.errors);
        assert_eq!(last.total_items, 6);
        assert_eq!(last.done_items, 6);
    }

    #[cfg(unix)]
    #[test]
    fn delete_removes_symlinks_not_their_targets() {
        let f = fx();
        let link = f.src.join("link");
        std::os::unix::fs::symlink(&f.dst, &link).unwrap();
        OpQueue::new()
            .start(req(FsOpKind::Delete, &[s(&f.src)], None), &Rec::default())
            .unwrap();
        assert!(!f.src.exists());
        assert_eq!(f.read("a.txt"), "old a", "link target untouched");
    }

    #[cfg(unix)]
    #[test]
    fn copy_reproduces_symlinks_rather_than_following_them() {
        let f = fx();
        std::os::unix::fs::symlink("a.txt", f.src.join("alias")).unwrap();
        let request = FsOpRequest {
            sources: vec![s(&f.src).to_string()],
            ..f.copy(ConflictPolicy::Skip)
        };
        OpQueue::new().start(request, &Rec::default()).unwrap();
        let copied = f.dst.join("src/alias");
        assert!(fs::symlink_metadata(&copied)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(copied).unwrap(), Path::new("a.txt"));
    }

    // ---- meter --------------------------------------------------------------------------

    #[test]
    fn meter_needs_a_warmup_before_estimating() {
        let mut m = Meter::default();
        m.resume();
        m.window_bytes = 1000;
        m.tick();
        assert!(!m.warmed_up(), "no window has elapsed yet");

        // One short window measures a rate but is not yet enough history for an ETA.
        m.window_start = Some(Instant::now() - RATE_WINDOW);
        m.tick();
        assert!(m.measured);
        assert!(m.bytes_rate > 0.0);
        assert!(!m.warmed_up());

        // Paused time does not count towards the warm-up.
        m.pause();
        m.resume();
        assert!(!m.warmed_up());

        m.window_start = Some(Instant::now() - ETA_WARMUP);
        m.tick();
        assert!(m.warmed_up());
    }
}
