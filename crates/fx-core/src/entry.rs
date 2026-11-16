//! Filesystem entry types.
//!
//! These are **IPC types**: what crosses the Rust↔WebView boundary. They are deliberately
//! flat, with no nested allocation beyond strings and no `Option` chains, because they are
//! serialised hundreds of thousands of times.
//!
//! Note for `fx-index`: do **not** store `FileEntry` as the in-memory index representation.
//! At ~80 bytes plus two heap strings per entry, a 5M-file volume would cost well over a
//! gigabyte. Keep an internal columnar/arena layout and materialise `FileEntry` only for
//! the rows actually being returned.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable per-volume identifier for an entry.
///
/// On Windows this is the NTFS File Reference Number straight from the MFT, which makes
/// parent lookups a plain integer comparison. On other platforms it is a synthetic id
/// assigned at scan time. Only unique *within* a volume — pair with [`VolumeId`] for a
/// globally unique key.
pub type EntryId = u64;

/// Identifies a mounted volume. `"C"` on Windows, `"/"` or a mount point elsewhere.
pub type VolumeId = String;

/// Unix epoch milliseconds. `0` means "unknown", which is cheaper to branch on than an
/// `Option` and serialises as a plain number.
pub type Millis = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Drive root, `\\?\Volume{...}`, or other shell pseudo-container.
    Volume,
    Unknown,
}

impl EntryKind {
    /// Whether this entry can be navigated into. Used for sort grouping (directories
    /// first) and for deciding whether a double-click opens or descends.
    pub fn is_container(self) -> bool {
        matches!(self, EntryKind::Dir | EntryKind::Volume)
    }
}

/// Filesystem attribute flags, expanded to booleans.
///
/// Stored as bools rather than a packed bitfield specifically because this is an IPC type:
/// it keeps the generated TypeScript readable and avoids every UI call site re-deriving bit
/// masks. Index internals should use a packed `u32`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Attributes {
    pub hidden: bool,
    pub system: bool,
    pub readonly: bool,
    pub archive: bool,
    pub compressed: bool,
    pub encrypted: bool,
    pub sparse: bool,
    /// NTFS reparse point, or a symlink/junction on other platforms.
    pub reparse: bool,
}

/// One row in a directory listing.
///
/// Carries no path: the caller already knows the directory it asked for, so repeating the
/// prefix on every row would be the single largest chunk of listing payload. Search results
/// use [`crate::SearchHit`] instead, which does carry a resolved path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileEntry {
    #[ts(type = "number")]
    pub id: EntryId,
    #[ts(type = "number")]
    pub parent: EntryId,
    /// File name including extension, no directory component.
    pub name: String,
    /// Lowercased extension without the dot, precomputed at scan time so extension
    /// filters never re-parse the name. `None` for directories and extensionless files.
    pub ext: Option<String>,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub modified: Millis,
    #[ts(type = "number")]
    pub created: Millis,
    pub kind: EntryKind,
    pub attrs: Attributes,
}

impl FileEntry {
    /// Splits a file name into its stem and lowercased extension.
    ///
    /// Leading-dot names (`.gitignore`) are treated as having no extension, matching
    /// Explorer's behaviour rather than `Path::extension`'s.
    pub fn split_ext(name: &str) -> (&str, Option<String>) {
        match name.rfind('.') {
            Some(i) if i > 0 && i + 1 < name.len() => {
                (&name[..i], Some(name[i + 1..].to_ascii_lowercase()))
            }
            _ => (name, None),
        }
    }
}

/// A mounted volume, for the sidebar and for scoping searches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VolumeInfo {
    pub id: VolumeId,
    /// Path to mount at, e.g. `C:\`.
    pub mount: String,
    /// Volume label, or a sensible fallback like "Local Disk".
    pub label: String,
    pub filesystem: Filesystem,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub free_bytes: u64,
    pub removable: bool,
    /// False for network shares and non-NTFS volumes, which cannot be MFT-indexed and
    /// therefore fall back to the directory-walking backend.
    pub indexable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Filesystem {
    /// The only filesystem supporting the MFT fast path.
    Ntfs,
    ReFS,
    Fat32,
    ExFat,
    /// macOS, used during development only.
    Apfs,
    Network,
    Other,
}

impl Filesystem {
    /// Whether the raw-MFT indexing path applies. Everything else degrades to `walkdir`.
    pub fn supports_mft(self) -> bool {
        matches!(self, Filesystem::Ntfs)
    }
}
