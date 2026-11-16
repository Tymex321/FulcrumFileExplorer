// `unwrap` is denied in production code because a panic inside a Tauri command takes
// down the whole webview, but in tests it is the clearest way to assert success.
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! # fx-fs — listing, file operations, thumbnails
//!
//! Implements [`fx_core::FsBackend`].
//!
//! ## The design goal is honesty
//!
//! Explorer's copy dialog is the canonical example of a progress bar nobody believes: it
//! jumps, stalls at 99%, and invents time estimates. The cause is that it starts copying
//! before it knows what it is copying.
//!
//! So every operation here runs two phases. [`fx_core::OpState::Scanning`] walks the
//! sources and counts items and bytes; only then does [`fx_core::OpState::Running`] begin,
//! with a real denominator. [`fx_core::Progress::eta_secs`] stays `None` during scanning
//! rather than guessing.
//!
//! ## Platform split
//!
//! | Module | Platform | Notes |
//! |---|---|---|
//! | [`ops`] | all | the copy/move/delete engine, queue, conflict handling |
//! | [`list`] | all | directory enumeration |
//! | [`windows_fs`] | Windows | `IFileOperation`, shell icons, context menus |
//! | [`unix_fs`] | macOS | development twin for everything above |
//!
//! Operation *logic* lives in the portable modules so it is testable on macOS; the
//! platform modules only supply OS-specific capability.

pub mod list;
pub mod ops;
pub mod thumbs;

#[cfg(windows)]
pub mod windows_fs;

#[cfg(not(windows))]
pub mod unix_fs;

use std::path::Path;

use fx_core::{
    BatchSink, FileEntry, FsBackend, FsOpRequest, ListOptions, ListSummary, OpControl, OpId,
    ProgressSink, Result, Thumbnail, VolumeInfo,
};

/// The platform filesystem backend.
#[derive(Debug, Default)]
pub struct PlatformFs {
    ops: ops::OpQueue,
}

impl PlatformFs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FsBackend for PlatformFs {
    fn list(
        &self,
        dir: &str,
        options: &ListOptions,
        sink: &dyn BatchSink<FileEntry>,
    ) -> Result<ListSummary> {
        list::list_dir(dir, options, sink)
    }

    fn stat(&self, path: &str) -> Result<FileEntry> {
        list::stat(path)
    }

    fn volumes(&self) -> Result<Vec<VolumeInfo>> {
        #[cfg(windows)]
        return windows_fs::volumes();
        #[cfg(not(windows))]
        return unix_fs::volumes();
    }

    fn create_dir(&self, path: &str) -> Result<()> {
        std::fs::create_dir_all(path).map_err(Into::into)
    }

    fn rename(&self, path: &str, new_name: &str) -> Result<()> {
        ops::rename(path, new_name)
    }

    fn start_op(&self, request: FsOpRequest, progress: &dyn ProgressSink) -> Result<OpId> {
        self.ops.start(request, progress)
    }

    fn control_op(&self, op: OpId, control: OpControl) -> Result<()> {
        self.ops.control(op, control)
    }

    fn thumbnail(&self, path: &str, size_px: u32) -> Result<Thumbnail> {
        thumbs::thumbnail(path, size_px)
    }

    fn icon(&self, path: &str, size_px: u32) -> Result<Thumbnail> {
        thumbs::icon(path, size_px)
    }

    fn open(&self, path: &str) -> Result<()> {
        #[cfg(windows)]
        return windows_fs::open(path);
        #[cfg(not(windows))]
        return unix_fs::open(path);
    }

    fn reveal(&self, path: &str) -> Result<()> {
        #[cfg(windows)]
        return windows_fs::reveal(path);
        #[cfg(not(windows))]
        return unix_fs::reveal(path);
    }
}

/// Sends one item to the platform's recycle bin / trash.
///
/// Windows uses `IFileOperation` and refuses outright where recycling is impossible
/// (network shares), rather than letting the shell fall back to permanent deletion.
pub(crate) fn trash_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    return windows_fs::recycle(path);
    #[cfg(not(windows))]
    return unix_fs::trash(path);
}
