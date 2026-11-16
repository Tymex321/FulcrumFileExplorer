//! An [`IndexBackend`] whose implementation can be replaced at runtime.
//!
//! The app starts on the walk index and, once the helper is installed or found, swaps to
//! the pipe client without anyone upstream holding a stale reference.

use std::sync::Arc;

use fx_core::{
    BatchSink, ChangeCallback, EntryId, IndexBackend, IndexStats, ProgressSink, Query,
    QuerySummary, Result, SearchHit, VolumeId, WatchHandle,
};

pub struct SwitchableIndex {
    inner: parking_lot::RwLock<Arc<dyn IndexBackend>>,
}

impl std::fmt::Debug for SwitchableIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SwitchableIndex")
    }
}

impl SwitchableIndex {
    pub fn new(initial: Arc<dyn IndexBackend>) -> Self {
        Self {
            inner: parking_lot::RwLock::new(initial),
        }
    }

    /// Replaces the backend. In-flight calls finish on the old one.
    pub fn swap(&self, next: Arc<dyn IndexBackend>) {
        *self.inner.write() = next;
    }

    // Clone the Arc so the lock is not held across a (possibly long) backend call.
    fn current(&self) -> Arc<dyn IndexBackend> {
        Arc::clone(&self.inner.read())
    }
}

impl IndexBackend for SwitchableIndex {
    fn stats(&self) -> IndexStats {
        self.current().stats()
    }
    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        self.current().scan(progress)
    }
    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
        self.current().query(query, sink)
    }
    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle> {
        self.current().watch(on_change)
    }
    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String> {
        self.current().resolve_path(volume, id)
    }
    fn clear(&self) -> Result<()> {
        self.current().clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::{IndexState, IndexStats};

    struct Fixed(IndexState);
    impl IndexBackend for Fixed {
        fn stats(&self) -> IndexStats {
            IndexStats {
                state: self.0,
                ..Default::default()
            }
        }
        fn scan(&self, _: &dyn ProgressSink) -> Result<()> {
            Ok(())
        }
        fn query(&self, _: &Query, _: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
            Ok(QuerySummary::default())
        }
        fn watch(&self, _: ChangeCallback) -> Result<WatchHandle> {
            Ok(WatchHandle::noop())
        }
        fn resolve_path(&self, _: &VolumeId, _: EntryId) -> Result<String> {
            Ok(String::new())
        }
        fn clear(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn swap_redirects_subsequent_calls() {
        let s = SwitchableIndex::new(Arc::new(Fixed(IndexState::Degraded)));
        assert_eq!(s.stats().state, IndexState::Degraded);
        s.swap(Arc::new(Fixed(IndexState::Ready)));
        assert_eq!(s.stats().state, IndexState::Ready);
    }
}
