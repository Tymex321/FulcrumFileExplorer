//! Directory listing, file operations, and thumbnails.
//!
//! The honesty requirement on progress drives the design here: Explorer's copy dialog is
//! distrusted because it guesses. Every operation therefore runs a scan phase that counts
//! items and bytes up front ([`OpState::Scanning`]), so the bar that follows is real.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::entry::{FileEntry, VolumeInfo};
use crate::error::Result;
use crate::query::Sort;
use crate::sink::{BatchSink, ProgressSink};

/// Identifies a running file operation. Allocated by the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OpId(#[ts(type = "number")] pub u64);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct ListOptions {
    pub show_hidden: bool,
    pub show_system: bool,
    /// Sorting happens in Rust so the UI can render a virtualised window directly without
    /// holding the full set in JS.
    pub sort: Sort,
    /// Compute directory sizes recursively. Off by default: it turns a listing from
    /// instant into a full subtree walk.
    pub compute_dir_sizes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListSummary {
    #[ts(type = "number")]
    pub total: u64,
    #[ts(type = "number")]
    pub elapsed_micros: u64,
    /// Entries skipped because of permissions, so the UI can note "3 items not shown".
    #[ts(type = "number")]
    pub inaccessible: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum FsOpKind {
    Copy,
    Move,
    /// Permanent deletion.
    Delete,
    /// Send to Recycle Bin.
    Trash,
}

/// What to do when a destination already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ConflictPolicy {
    /// Pause and ask the UI. The operation sits in [`OpState::Paused`] until resolved.
    #[default]
    Ask,
    Skip,
    Overwrite,
    /// Append " (2)", " (3)", … matching Explorer's naming.
    Rename,
    /// Overwrite only when the source is newer. Useful for folder syncing.
    KeepNewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FsOpRequest {
    pub kind: FsOpKind,
    pub sources: Vec<String>,
    /// Destination directory. Ignored for `Delete` and `Trash`.
    pub destination: Option<String>,
    pub conflict: ConflictPolicy,
    /// Verify by re-reading and comparing after copy. Roughly halves throughput, so it is
    /// opt-in per operation.
    pub verify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum OpState {
    #[default]
    Pending,
    /// Counting items and bytes so the subsequent progress bar is accurate.
    Scanning,
    Running,
    /// Either user-paused or waiting on a [`ConflictPolicy::Ask`] decision.
    Paused,
    Cancelled,
    Done,
    Failed,
}

impl OpState {
    pub fn is_terminal(self) -> bool {
        matches!(self, OpState::Cancelled | OpState::Done | OpState::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OpError {
    pub path: String,
    pub message: String,
}

/// A snapshot of operation progress. Emitted through [`ProgressSink`], throttled at the
/// IPC boundary to ~10/sec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Progress {
    pub op: OpId,
    pub kind: FsOpKind,
    pub state: OpState,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub done_bytes: u64,
    #[ts(type = "number")]
    pub total_items: u64,
    #[ts(type = "number")]
    pub done_items: u64,
    /// Path currently being worked on, for the detail line under the bar.
    pub current: String,
    #[ts(type = "number")]
    pub bytes_per_sec: u64,
    /// `None` while still scanning, since an estimate before the total is known would be
    /// the exact dishonesty this design is avoiding.
    #[ts(type = "number | null")]
    pub eta_secs: Option<u64>,
    /// Non-fatal per-file failures. The operation continues; these are reported at the end.
    pub errors: Vec<OpError>,
    /// Set when `state == Paused` because of an unresolved conflict.
    pub pending_conflict: Option<Conflict>,
}

/// A conflict awaiting a decision from the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Conflict {
    pub source: String,
    pub destination: String,
    #[ts(type = "number")]
    pub source_size: u64,
    #[ts(type = "number")]
    pub dest_size: u64,
    #[ts(type = "number")]
    pub source_modified: i64,
    #[ts(type = "number")]
    pub dest_modified: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum OpControl {
    Pause,
    Resume,
    Cancel,
    /// Resolve the pending conflict. `apply_to_all` avoids prompting once per file in a
    /// 10,000-file copy.
    Resolve {
        policy: ConflictPolicy,
        apply_to_all: bool,
    },
}

/// An encoded image for a thumbnail or icon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Thumbnail {
    /// PNG or JPEG bytes, base64-encoded for transport. Large enough that callers should
    /// request these lazily, only for rows actually on screen.
    pub data_base64: String,
    pub width: u32,
    pub height: u32,
    /// True when this is a generic file-type icon rather than real content, so the UI can
    /// avoid caching it as if it were a thumbnail.
    pub is_fallback: bool,
}

pub trait FsBackend: Send + Sync {
    /// Lists a directory, streaming batches so huge directories paint immediately.
    fn list(
        &self,
        dir: &str,
        options: &ListOptions,
        sink: &dyn BatchSink<FileEntry>,
    ) -> Result<ListSummary>;

    fn stat(&self, path: &str) -> Result<FileEntry>;

    fn volumes(&self) -> Result<Vec<VolumeInfo>>;

    fn create_dir(&self, path: &str) -> Result<()>;

    fn rename(&self, path: &str, new_name: &str) -> Result<()>;

    /// Starts a long-running operation and returns immediately with its id. Progress
    /// arrives on `progress`; control it afterwards via [`FsBackend::control_op`].
    fn start_op(&self, request: FsOpRequest, progress: &dyn ProgressSink) -> Result<OpId>;

    fn control_op(&self, op: OpId, control: OpControl) -> Result<()>;

    /// Content thumbnail. Returns a fallback icon when the type has no preview.
    fn thumbnail(&self, path: &str, size_px: u32) -> Result<Thumbnail>;

    /// File-type icon. Cheaper than [`FsBackend::thumbnail`] and used for list rows.
    fn icon(&self, path: &str, size_px: u32) -> Result<Thumbnail>;

    /// Opens with the OS default handler.
    fn open(&self, path: &str) -> Result<()>;

    /// Reveals in the OS file manager (Explorer / Finder).
    fn reveal(&self, path: &str) -> Result<()>;
}
