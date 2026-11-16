// `unwrap` is denied in production code because a panic inside a Tauri command takes
// down the whole webview, but in tests it is the clearest way to assert success.
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! # fx-index — the search index
//!
//! This is the crate that makes Fulcrum worth building.
//!
//! ## Why two backends
//!
//! Windows Explorer searches slowly because it walks directories and defers to the Windows
//! Search indexer. The fast path instead reads the NTFS Master File Table directly off the
//! raw volume — one sequential read that yields every file record on the disk — and then
//! stays current by replaying the USN change journal. That is what makes a whole-volume
//! filename search finish in microseconds.
//!
//! That path is Windows-only, needs administrator rights, and cannot run on the macOS
//! machine this is being developed on. So it sits behind [`fx_core::IndexBackend`]
//! alongside a `walkdir` implementation, and nothing upstream knows which one it has:
//!
//! | Backend | Platform | Used when |
//! |---|---|---|
//! | `mft` | Windows, elevated | NTFS volume and the helper service is installed |
//! | [`walk`] | all | development on macOS, non-NTFS volumes, or before the helper exists |
//!
//! The unelevated app reaches the MFT backend through [`ipc::HelperClient`], which
//! implements the same trait by forwarding over the helper's pipe. [`default_backend`]
//! picks the best of the three.
//!
//! Everything both backends share — the columnar [`store`], the [`query`] engine, and the
//! MFT/USN decision logic in [`ntfs`] — is platform-independent, so the macOS test suite
//! covers the code that answers queries on Windows.
//!
//! ## Memory discipline
//!
//! A 5-million-file volume is normal. Storing [`fx_core::FileEntry`] per file — two heap
//! strings each — would cost over a gigabyte and make the index a liability rather than a
//! feature. The internal representation is therefore columnar (see [`store`]): names in
//! one shared arena addressed by `(offset, len)` with a case-folded mirror, parents as
//! `u32` record indices, attributes packed into a `u16` — a 40-byte record per entry.
//! Full paths are never stored; they are reconstructed by walking the parent chain, and
//! only for rows actually returned.

pub mod walk;

#[cfg(windows)]
pub mod mft;

#[cfg(windows)]
pub mod usn;

pub mod ipc;
pub mod ntfs;
pub mod query;
pub mod store;

pub use walk::WalkIndex;

#[cfg(windows)]
pub use mft::MftIndex;

use fx_core::{FsOpKind, Millis, OpId, OpState, Progress};

/// Debounce window for coalescing change notifications before invoking a
/// [`fx_core::ChangeCallback`], shared by the USN watcher and the walk backend's
/// filesystem watcher.
///
/// Short enough that changes feel immediate, long enough to collapse the burst from a bulk
/// file operation (an `npm install` emits tens of thousands of records a second) into one
/// update.
pub const COALESCE_WINDOW_MS: u64 = 100;

/// [`OpId`] carried by index-scan progress. Real file operations are numbered from 1 by
/// `fx-fs`, so the top value can never collide with one.
pub const INDEX_SCAN_OP: OpId = OpId(u64::MAX);

/// Progress snapshot for an index scan.
///
/// `fx_core::Progress` was designed for file operations and has no indexing kind;
/// scans are identified by [`INDEX_SCAN_OP`] instead, and
/// `kind` is a placeholder the UI should ignore for that id. `total_items` is `0` while the
/// total is unknown (the walk backend), which keeps `eta_secs` honestly `None`.
pub(crate) fn scan_progress(state: OpState, done: u64, total: u64, current: String) -> Progress {
    Progress {
        op: INDEX_SCAN_OP,
        kind: FsOpKind::Copy,
        state,
        total_bytes: 0,
        done_bytes: 0,
        total_items: total,
        done_items: done,
        current,
        bytes_per_sec: 0,
        eta_secs: None,
        errors: Vec::new(),
        pending_conflict: None,
    }
}

/// Converts a filesystem timestamp to [`Millis`], keeping pre-1970 times negative rather
/// than collapsing them to "unknown".
pub(crate) fn system_time_millis(t: std::time::SystemTime) -> Millis {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as Millis,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as Millis),
    }
}

/// Chooses the best available backend for this platform and privilege level.
///
/// In order of preference:
///
/// 1. **The helper service** (Windows): instant search over every NTFS volume, without
///    this process being elevated. `stats().helper_connected` is `true`.
/// 2. **The MFT read in-process** (Windows, only if this process is itself elevated —
///    a developer running as administrator).
/// 3. **The walk backend** everywhere else: macOS, and Windows before the helper is
///    installed. It reports [`fx_core::IndexState::Degraded`], which is the UI's cue to
///    offer "Enable instant search" — so search always works to some degree and never
///    fails outright.
///
/// Never prompts for elevation and never blocks for long: a missing helper is detected
/// by a failed pipe open.
pub fn default_backend() -> Box<dyn fx_core::IndexBackend> {
    #[cfg(windows)]
    {
        match ipc::HelperClient::connect_pipe() {
            Ok(client) => return Box::new(client),
            Err(e) => tracing::info!(error = %e, "index helper not available"),
        }
        let mft = MftIndex::all_volumes();
        if !mft.volumes().is_empty() && mft.is_available() {
            return Box::new(mft);
        }
    }
    Box::new(WalkIndex::new())
}
