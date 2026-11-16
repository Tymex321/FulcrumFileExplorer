//! Search query description and results.
//!
//! All filtering, ranking and sorting happens in Rust. The UI never post-processes a
//! result set — at a million candidates, shipping unfiltered rows into JS to sort them
//! there is exactly the mistake that would make this app feel like Explorer.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::entry::{EntryKind, FileEntry, Millis, VolumeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum MatchMode {
    /// Plain substring. The fast path, and the default: it can be answered by a
    /// vectorised scan over the name arena without scoring.
    #[default]
    Substring,
    /// Subsequence matching with ranking, for the `Ctrl+P` jump box.
    Fuzzy,
    Regex,
    /// Shell-style `*` / `?` wildcards, which users expect to work in a file manager.
    Glob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CaseMode {
    /// Case-sensitive only if the query contains an uppercase character. What users
    /// actually want, almost always.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum SearchScope {
    /// Every indexed volume.
    #[default]
    All,
    Volume(VolumeId),
    /// Restrict to a directory subtree. Still answered from the index, not by walking.
    Subtree(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SizeRange {
    #[ts(type = "number | null")]
    pub min: Option<u64>,
    #[ts(type = "number | null")]
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TimeRange {
    #[ts(type = "number | null")]
    pub from: Option<Millis>,
    #[ts(type = "number | null")]
    pub to: Option<Millis>,
}

/// Post-match predicates. An empty collection always means "no constraint" rather than
/// "match nothing", so a default-constructed `Filters` matches everything.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Filters {
    pub kinds: Vec<EntryKind>,
    /// Lowercase, no leading dot. Compared against the precomputed `FileEntry::ext`.
    pub extensions: Vec<String>,
    pub size: Option<SizeRange>,
    pub modified: Option<TimeRange>,
    pub created: Option<TimeRange>,
    pub include_hidden: bool,
    pub include_system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Created,
    Kind,
    Extension,
    Path,
    /// Only meaningful for [`MatchMode::Fuzzy`]; falls back to `Name` otherwise.
    Relevance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
    /// Group containers above files regardless of sort key — Explorer's default and what
    /// users expect when sorting by size or date.
    pub dirs_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            desc: false,
            dirs_first: true,
        }
    }
}

/// A search request.
///
/// `limit`/`offset` exist because the UI is virtualised: it asks for the window it is about
/// to paint plus a small buffer, never the whole result set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Query {
    pub text: String,
    pub mode: MatchMode,
    pub case: CaseMode,
    pub scope: SearchScope,
    pub filters: Filters,
    pub sort: Sort,
    /// Hard cap on returned rows. `0` is treated as [`Query::DEFAULT_LIMIT`].
    pub limit: u32,
    pub offset: u32,
    /// Also match against the full path, not just the file name. Costs a path
    /// reconstruction per candidate, so it is opt-in.
    pub match_full_path: bool,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            text: String::new(),
            mode: MatchMode::default(),
            case: CaseMode::default(),
            scope: SearchScope::default(),
            filters: Filters::default(),
            sort: Sort::default(),
            limit: Self::DEFAULT_LIMIT,
            offset: 0,
            match_full_path: false,
        }
    }
}

impl Query {
    pub const DEFAULT_LIMIT: u32 = 1_000;

    /// Rows per streamed batch. Tuned to keep each IPC message small enough that the first
    /// batch paints well inside one frame, while amortising per-message overhead.
    pub const BATCH_SIZE: usize = 500;

    pub fn effective_limit(&self) -> u32 {
        if self.limit == 0 {
            Self::DEFAULT_LIMIT
        } else {
            self.limit
        }
    }

    /// Resolves [`CaseMode::Smart`] against the query text.
    pub fn is_case_sensitive(&self) -> bool {
        match self.case {
            CaseMode::Sensitive => true,
            CaseMode::Insensitive => false,
            CaseMode::Smart => self.text.chars().any(|c| c.is_uppercase()),
        }
    }
}

/// A search result row. Unlike [`FileEntry`] this carries a resolved absolute path,
/// because a search result is not scoped to a directory the caller already knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchHit {
    pub entry: FileEntry,
    pub volume: VolumeId,
    /// Absolute path, reconstructed on demand from the parent chain.
    pub path: String,
    /// Higher is better. Always `0` outside [`MatchMode::Fuzzy`].
    pub score: u32,
    /// Byte offsets into `entry.name` (or `path` when `match_full_path` was set) that
    /// matched, so the UI can bold them without re-running the matcher in JS.
    #[ts(type = "number[]")]
    pub match_ranges: Vec<u32>,
}

/// Returned once a query finishes streaming, to tell the UI what it just received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuerySummary {
    /// Rows actually delivered, after limit/offset.
    #[ts(type = "number")]
    pub returned: u64,
    /// Total matches found before limiting, so the UI can say "1,203 results".
    #[ts(type = "number")]
    pub total_matched: u64,
    #[ts(type = "number")]
    pub elapsed_micros: u64,
    /// True when the match set was capped early and `total_matched` is a lower bound.
    pub truncated: bool,
    /// True when answered by the walkdir fallback rather than the MFT index, which the UI
    /// surfaces as a hint that instant search is not enabled yet.
    pub degraded: bool,
}
