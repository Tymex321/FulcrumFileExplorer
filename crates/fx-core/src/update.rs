//! What the app tells the UI about an available update.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A newer version found by the update check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateInfo {
    /// The version on offer, e.g. `1.0.2`.
    pub version: String,
    /// The version currently running.
    pub current_version: String,
    /// Release notes, when the release has any.
    pub notes: Option<String>,
}
