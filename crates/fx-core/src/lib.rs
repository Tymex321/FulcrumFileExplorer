// `unwrap` is denied in production code because a panic inside a Tauri command takes
// down the whole webview, but in tests it is the clearest way to assert success.
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! # fx-core — the contract
//!
//! Shared types and the two backend traits that the rest of Fulcrum is built on.
//!
//! ## Shape of the system
//!
//! ```text
//!   UI (React)          src-tauri            fx-index  ──pipe──> fx-helper (elevated)
//!   virtual list   ←──  commands + state  ←──  IndexBackend         MFT + USN journal
//!   purple theme        session, settings      fx-fs
//!                                              FsBackend
//! ```
//!
//! Two design rules run through everything here, both aimed at the same goal of the app
//! feeling instant:
//!
//! 1. **Results stream.** Listing and search return through [`sink::BatchSink`] so the
//!    first rows of a 500k-entry directory paint in one frame instead of after the last
//!    entry is read.
//! 2. **Rust does the work.** Filtering, ranking and sorting all happen behind these
//!    traits. The UI renders a window over an already-ordered result set and never
//!    post-processes rows in JavaScript.
//!
//! ## TypeScript bindings
//!
//! Every IPC type derives [`ts_rs::TS`], and `cargo test -p fx-core` regenerates
//! `ui/src/bindings/`. The UI imports those generated types rather than hand-writing
//! interfaces, which turns a Rust/TS mismatch into a compile error instead of a runtime
//! surprise. Regenerate with `scripts/gen-bindings.sh`.
//!
//! Note that `u64`/`i64` fields are annotated `#[ts(type = "number")]`: ts-rs would
//! otherwise map them to `bigint`, but Tauri's JSON bridge delivers plain JS numbers.

pub mod entry;
pub mod error;
pub mod fsops;
pub mod index;
pub mod query;
pub mod settings;
pub mod sink;
pub mod update;

pub use entry::{
    Attributes, EntryId, EntryKind, FileEntry, Filesystem, Millis, VolumeId, VolumeInfo,
};
pub use error::{FxError, Result};
pub use fsops::{
    Conflict, ConflictPolicy, FsBackend, FsOpKind, FsOpRequest, ListOptions, ListSummary,
    OpControl, OpError, OpId, OpState, Progress, Thumbnail,
};
pub use index::{ChangeCallback, ChangeEvent, IndexBackend, IndexState, IndexStats, WatchHandle};
pub use query::{
    CaseMode, Filters, MatchMode, Query, QuerySummary, SearchHit, SearchScope, SizeRange, Sort,
    SortKey, TimeRange,
};
pub use settings::{
    Accent, Appearance, BehaviorSettings, Density, IndexSettings, PaneLayout, SessionState,
    Settings, TabState, ViewMode,
};
pub use sink::{BatchSink, CollectSink, NullSink, ProgressSink};
pub use update::UpdateInfo;

/// Named pipe the elevated helper listens on.
///
/// The helper runs as a Windows service holding raw `\\.\C:` access; the unelevated app
/// connects here. Defined in `fx-core` so both sides cannot disagree about it.
pub const HELPER_PIPE_NAME: &str = r"\\.\pipe\fulcrum-index";

/// Wire protocol version for the helper pipe. Bump on any breaking change to the
/// request/response types so a stale installed service is rejected rather than
/// misinterpreted.
pub const HELPER_PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ext_handles_dotfiles_like_explorer() {
        assert_eq!(
            FileEntry::split_ext("notes.TXT"),
            ("notes", Some("txt".into()))
        );
        assert_eq!(
            FileEntry::split_ext("archive.tar.gz"),
            ("archive.tar", Some("gz".into()))
        );
        // Leading-dot names are extensionless, unlike `Path::extension`.
        assert_eq!(FileEntry::split_ext(".gitignore"), (".gitignore", None));
        assert_eq!(FileEntry::split_ext("Makefile"), ("Makefile", None));
        // A trailing dot leaves no extension to speak of.
        assert_eq!(FileEntry::split_ext("weird."), ("weird.", None));
    }

    #[test]
    fn smart_case_follows_query_text() {
        let lower = Query {
            text: "readme".into(),
            ..Default::default()
        };
        assert!(!lower.is_case_sensitive());

        let mixed = Query {
            text: "ReadMe".into(),
            ..Default::default()
        };
        assert!(mixed.is_case_sensitive());
    }

    #[test]
    fn zero_limit_means_default_not_nothing() {
        let q = Query {
            limit: 0,
            ..Default::default()
        };
        assert_eq!(q.effective_limit(), Query::DEFAULT_LIMIT);
    }

    #[test]
    fn default_filters_match_everything() {
        let f = Filters::default();
        assert!(f.kinds.is_empty());
        assert!(f.extensions.is_empty());
        assert!(f.size.is_none());
    }

    #[test]
    fn io_errors_map_to_distinct_variants() {
        use std::io::{Error, ErrorKind};
        assert!(matches!(
            FxError::from(Error::new(ErrorKind::NotFound, "x")),
            FxError::NotFound(_)
        ));
        assert!(matches!(
            FxError::from(Error::new(ErrorKind::PermissionDenied, "x")),
            FxError::AccessDenied(_)
        ));
        // Cancellation must not be reported to the user as a failure.
        assert!(!FxError::Cancelled.is_user_visible());
    }

    #[test]
    fn collect_sink_accumulates_batches() {
        let sink = CollectSink::<u32>::new();
        assert!(sink.push(vec![1, 2]));
        assert!(sink.push(vec![3]));
        assert_eq!(sink.into_inner(), vec![1, 2, 3]);
    }
}
