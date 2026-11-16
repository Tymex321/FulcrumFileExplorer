//! Bridges [`fx_core`]'s synchronous sinks onto Tauri channels.
//!
//! Backends are synchronous and run on `spawn_blocking`; the UI wants a stream. These
//! adapters sit between, and they are where the two throttling rules live:
//!
//! - **Batches, not rows.** A listing pushes [`fx_core::Query::BATCH_SIZE`] entries per
//!   message. Per-row IPC would spend more time in serialisation than in the filesystem.
//! - **Progress is rate-limited.** A copy loop emits progress thousands of times a second;
//!   only ~10/sec reach the UI. Terminal states are always forwarded, never dropped, or the
//!   UI would show a job stuck at 99%.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fx_core::{BatchSink, Progress, ProgressSink};
use tauri::ipc::Channel;

/// Forwards batches to the UI over a Tauri channel.
pub struct ChannelSink<T: Clone + Send + Sync + serde::Serialize + 'static> {
    channel: Channel<Vec<T>>,
    /// Set once the UI disconnects, so the producer stops instead of finishing work that
    /// nobody is going to render.
    cancelled: AtomicBool,
}

impl<T: Clone + Send + Sync + serde::Serialize + 'static> std::fmt::Debug for ChannelSink<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSink")
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T: Clone + Send + Sync + serde::Serialize + 'static> ChannelSink<T> {
    pub fn new(channel: Channel<Vec<T>>) -> Self {
        Self {
            channel,
            cancelled: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl<T: Clone + Send + Sync + serde::Serialize + 'static> BatchSink<T> for ChannelSink<T> {
    fn push(&self, batch: Vec<T>) -> bool {
        if self.cancelled.load(Ordering::Relaxed) {
            return false;
        }
        match self.channel.send(batch) {
            Ok(()) => true,
            Err(_) => {
                // The receiving side is gone; tell the producer to stop.
                self.cancel();
                false
            }
        }
    }
}

/// Minimum gap between progress messages reaching the UI.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Rate-limits progress updates onto a channel.
pub struct ThrottledProgress {
    channel: Channel<Progress>,
    last: parking_lot::Mutex<Instant>,
    cancelled: AtomicBool,
}

impl std::fmt::Debug for ThrottledProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThrottledProgress").finish()
    }
}

impl ThrottledProgress {
    pub fn new(channel: Channel<Progress>) -> Self {
        Self {
            channel,
            // Dated so the very first update is always delivered immediately.
            last: parking_lot::Mutex::new(Instant::now() - PROGRESS_INTERVAL),
            cancelled: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl ProgressSink for ThrottledProgress {
    fn emit(&self, progress: &Progress) {
        // Terminal states bypass the throttle: dropping the final update would leave the
        // UI showing a job that never finishes.
        let terminal = progress.state.is_terminal();
        if !terminal {
            let mut last = self.last.lock();
            if last.elapsed() < PROGRESS_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let _ = self.channel.send(progress.clone());
    }

    fn should_continue(&self) -> bool {
        !self.cancelled.load(Ordering::Relaxed)
    }
}
