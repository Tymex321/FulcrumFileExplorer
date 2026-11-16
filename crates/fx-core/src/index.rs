//! The search index contract.
//!
//! Two implementations exist behind this trait and the rest of the app cannot tell them
//! apart:
//!
//! - **MFT backend** (Windows, elevated): reads `\\.\C:` raw, parses the Master File Table
//!   into a compact in-memory index, and stays current by replaying the USN change
//!   journal. Microsecond queries over millions of files.
//! - **Walk backend** (macOS development, and Windows before the helper is installed, and
//!   any non-NTFS volume): `walkdir` over a limited set of roots, kept fresh with
//!   filesystem notifications. Slower and narrower, but the same API.
//!
//! Keeping both behind one trait is what makes this project developable on a Mac at all.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::entry::{EntryId, VolumeId};
use crate::error::Result;
use crate::query::{Query, QuerySummary, SearchHit};
use crate::sink::{BatchSink, ProgressSink};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum IndexState {
    /// No index yet. On Windows this is where the "Enable instant search" prompt lives.
    #[default]
    Idle,
    /// First full scan in progress. Queries are answerable but incomplete.
    Scanning,
    /// Fully built and being kept live.
    Ready,
    /// Running on the walkdir fallback: queries work but cover only indexed roots.
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IndexStats {
    pub state: IndexState,
    #[ts(type = "number")]
    pub total_entries: u64,
    #[ts(type = "number")]
    pub total_dirs: u64,
    /// Resident size of the index. Surfaced in settings because users of Everything-style
    /// tools reasonably care how much RAM the index costs them.
    #[ts(type = "number")]
    pub memory_bytes: u64,
    #[ts(type = "number")]
    pub last_scan_millis: u64,
    pub volumes: Vec<VolumeId>,
    /// Whether the privileged helper is connected. False means MFT search is unavailable.
    pub helper_connected: bool,
}

/// How an indexed entry changed. Produced by the USN journal on Windows and by filesystem
/// notifications elsewhere, then applied incrementally — a change must never trigger a
/// rescan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ChangeEvent {
    Created {
        volume: VolumeId,
        #[ts(type = "number")]
        id: EntryId,
    },
    Deleted {
        volume: VolumeId,
        #[ts(type = "number")]
        id: EntryId,
    },
    Modified {
        volume: VolumeId,
        #[ts(type = "number")]
        id: EntryId,
    },
    Renamed {
        volume: VolumeId,
        #[ts(type = "number")]
        id: EntryId,
        old_name: String,
        new_name: String,
    },
    /// The USN journal wrapped or was reset and deltas were lost, so the index for this
    /// volume is stale and must be rebuilt. Rare but must be handled.
    Desynced { volume: VolumeId },
}

/// Cancels a watch when dropped.
pub struct WatchHandle {
    stop: Box<dyn FnOnce() + Send>,
}

impl std::fmt::Debug for WatchHandle {
    // The boxed closure is not inspectable, so report the type rather than its contents.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WatchHandle")
    }
}

impl WatchHandle {
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            stop: Box::new(stop),
        }
    }

    /// A handle that does nothing, for backends without change notification.
    pub fn noop() -> Self {
        Self::new(|| {})
    }

    pub fn stop(self) {
        (self.stop)();
    }
}

/// Callback invoked with coalesced batches of change events.
pub type ChangeCallback = Box<dyn Fn(Vec<ChangeEvent>) + Send + Sync + 'static>;

pub trait IndexBackend: Send + Sync {
    fn stats(&self) -> IndexStats;

    /// Builds or rebuilds the index, reporting progress as it goes. Expected to be the
    /// slow call: tens of seconds for a large volume on first run.
    fn scan(&self, progress: &dyn ProgressSink) -> Result<()>;

    /// Answers a query, streaming hits in [`Query::BATCH_SIZE`] batches.
    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary>;

    /// Starts watching for changes. Events must be coalesced before invoking `on_change` —
    /// an unpacking archive can generate tens of thousands of USN records a second and
    /// each one must not become a UI update.
    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle>;

    /// Reconstructs an absolute path from the parent chain. Paths are not stored per entry,
    /// so this is a walk up the tree rather than a field read.
    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String>;

    /// Drops the index and releases its memory.
    fn clear(&self) -> Result<()>;
}
