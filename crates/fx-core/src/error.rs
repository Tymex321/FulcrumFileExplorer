//! Error type crossing both the IPC boundary and the privileged-helper pipe.
//!
//! Every variant carries owned `String` data rather than wrapping source errors, so the
//! whole type stays `Serialize` + `Clone`. Backends should convert at the boundary.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

pub type Result<T> = std::result::Result<T, FxError>;

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "detail")]
pub enum FxError {
    /// The operation is not supported by this backend or build.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("access denied: {0}")]
    AccessDenied(String),

    /// Raw volume access was attempted without administrator rights. The UI turns this
    /// into the one-time "Enable instant search" elevation prompt rather than an error
    /// toast, so it must stay distinguishable from a plain `AccessDenied`.
    #[error("administrator rights required: {0}")]
    NotElevated(String),

    /// Volume is not NTFS, so there is no MFT to read. Caller should fall back to the
    /// directory-walking backend instead of surfacing this to the user.
    #[error("filesystem does not support indexing: {0}")]
    UnsupportedFilesystem(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("already exists: {0}")]
    AlreadyExists(String),

    /// Cooperative cancellation, not a failure. The UI should stay silent on this.
    #[error("cancelled")]
    Cancelled,

    #[error("io error: {0}")]
    Io(String),

    #[error("index error: {0}")]
    Index(String),

    /// The privileged helper service is unreachable or speaking the wrong protocol.
    #[error("helper unavailable: {0}")]
    HelperUnavailable(String),

    #[error("invalid query: {0}")]
    BadQuery(String),

    #[error("{0}")]
    Other(String),
}

impl FxError {
    /// Whether the operation is worth retrying unchanged.
    pub fn is_transient(&self) -> bool {
        matches!(self, FxError::Io(_) | FxError::HelperUnavailable(_))
    }

    /// Whether this should be shown to the user at all. Cancellation is user-initiated and
    /// a missing helper has its own dedicated UI affordance.
    pub fn is_user_visible(&self) -> bool {
        !matches!(self, FxError::Cancelled)
    }
}

impl From<std::io::Error> for FxError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => FxError::NotFound(e.to_string()),
            K::PermissionDenied => FxError::AccessDenied(e.to_string()),
            K::AlreadyExists => FxError::AlreadyExists(e.to_string()),
            K::Interrupted => FxError::Cancelled,
            _ => FxError::Io(e.to_string()),
        }
    }
}
