//! Streaming callbacks.
//!
//! Backends are **synchronous** on purpose. Directory listing, MFT scanning and fuzzy
//! matching are all CPU-bound, so they belong on a blocking thread pool rather than in an
//! async executor; the Tauri layer wraps calls in `spawn_blocking`. Keeping the traits sync
//! also keeps them object-safe without `async_trait`, and lets the same trait be
//! implemented on the far side of the helper's named pipe.
//!
//! Results come back through these sinks rather than as a returned `Vec` so the UI can
//! paint the first rows of a 500k-entry directory immediately instead of waiting for the
//! last one.

use crate::fsops::Progress;

/// Receives results in batches as they are produced.
///
/// Implementations must be cheap to call and must not block for long — a slow sink
/// throttles the producer directly.
pub trait BatchSink<T>: Send + Sync {
    /// Hands over one batch. Returns `false` if the consumer has gone away or the user
    /// navigated elsewhere, which is the producer's signal to stop early and return
    /// [`crate::FxError::Cancelled`].
    #[must_use]
    fn push(&self, batch: Vec<T>) -> bool;

    /// Optional hint of the total count, when known before streaming starts, so the UI can
    /// size its scrollbar correctly on the first batch.
    fn hint_total(&self, _total: u64) {}
}

/// Receives progress updates for a long-running file operation.
///
/// Implementations are expected to coalesce: a byte-level copy loop may call this
/// thousands of times per second, and every one of those must not become an IPC message.
/// Throttle to ~10/sec per operation at the boundary.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, progress: &Progress);

    /// Whether the operation should keep going. Checked periodically by producers to
    /// implement cooperative cancellation.
    fn should_continue(&self) -> bool {
        true
    }
}

/// A sink that collects everything into a `Vec`. For tests and for the few call sites that
/// genuinely want the whole result set.
pub struct CollectSink<T> {
    items: std::sync::Mutex<Vec<T>>,
}

impl<T> std::fmt::Debug for CollectSink<T> {
    // Reports the count rather than the contents, so `T` need not be `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectSink")
            .field("len", &self.len())
            .finish()
    }
}

impl<T> CollectSink<T> {
    pub fn new() -> Self {
        Self {
            items: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn into_inner(self) -> Vec<T> {
        self.items.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    pub fn len(&self) -> usize {
        self.items.lock().map(|v| v.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Default for CollectSink<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + Sync> BatchSink<T> for CollectSink<T> {
    fn push(&self, batch: Vec<T>) -> bool {
        match self.items.lock() {
            Ok(mut v) => {
                v.extend(batch);
                true
            }
            Err(_) => false,
        }
    }
}

/// Discards everything. Useful when a caller wants only the summary, e.g. counting matches.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl<T: Send + Sync> BatchSink<T> for NullSink {
    fn push(&self, _batch: Vec<T>) -> bool {
        true
    }
}

impl ProgressSink for NullSink {
    fn emit(&self, _progress: &Progress) {}
}
