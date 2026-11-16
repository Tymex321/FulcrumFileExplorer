//! Application state.
//!
//! Holds the two backends behind their traits so the app is identical whether it is
//! running the real MFT index on Windows or the development fallbacks on macOS. Swapping
//! the stubs for the real implementations is a change to [`AppState::new`] and nothing
//! else.

use std::path::PathBuf;
use std::sync::Arc;

use fx_core::{FsBackend, IndexBackend, SessionState, Settings};

use crate::persist::{self, DebouncedWriter, DEBOUNCE};
use crate::switchable::SwitchableIndex;

const SETTINGS_FILE: &str = "settings.json";
const SESSION_FILE: &str = "session.json";

pub struct AppState {
    /// Concrete type kept so `enable_instant_search` can swap the backend underneath.
    pub switch: Arc<SwitchableIndex>,
    pub index: Arc<dyn IndexBackend>,
    pub fs: Arc<dyn FsBackend>,
    pub settings: parking_lot::RwLock<Settings>,
    pub session: parking_lot::RwLock<SessionState>,
    settings_writer: Option<DebouncedWriter<Settings>>,
    session_writer: Option<DebouncedWriter<SessionState>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The trait objects are not Debug, so report the index state instead, which is
        // the part anyone debugging actually wants.
        f.debug_struct("AppState")
            .field("index_state", &self.index.stats().state)
            .finish()
    }
}

impl AppState {
    /// In-memory state with nothing persisted. Used by tests.
    pub fn new() -> Self {
        Self::build(None, Settings::default(), SessionState::default())
    }

    /// State persisted under `config_dir`, restoring the previous session when allowed.
    pub fn load(config_dir: PathBuf) -> Self {
        let settings: Settings = persist::load(&config_dir.join(SETTINGS_FILE)).unwrap_or_default();
        let session = if settings.behavior.restore_session {
            persist::load(&config_dir.join(SESSION_FILE)).unwrap_or_default()
        } else {
            SessionState::default()
        };
        Self::build(Some(config_dir), settings, session)
    }

    fn build(dir: Option<PathBuf>, settings: Settings, session: SessionState) -> Self {
        // With instant search on, `default_backend` uses the elevated helper if it is
        // installed and answering, and falls back to the in-process walk index otherwise,
        // so search always works and startup never fails on the helper's account.
        let initial: Arc<dyn IndexBackend> = if settings.index.instant_search {
            Arc::from(fx_index::default_backend())
        } else {
            Arc::new(fx_index::WalkIndex::new())
        };
        let switch = Arc::new(SwitchableIndex::new(initial));
        Self {
            index: switch.clone(),
            switch,
            fs: Arc::new(fx_fs::PlatformFs::new()),
            settings_writer: dir
                .as_ref()
                .map(|d| DebouncedWriter::new(d.join(SETTINGS_FILE), DEBOUNCE)),
            session_writer: dir.map(|d| DebouncedWriter::new(d.join(SESSION_FILE), DEBOUNCE)),
            settings: parking_lot::RwLock::new(settings),
            session: parking_lot::RwLock::new(session),
        }
    }

    pub fn update_settings(&self, settings: Settings) {
        *self.settings.write() = settings.clone();
        if let Some(w) = &self.settings_writer {
            w.save(settings);
        }
    }

    pub fn update_session(&self, session: SessionState) {
        *self.session.write() = session.clone();
        if let Some(w) = &self.session_writer {
            w.save(session);
        }
    }

    /// Writes anything still waiting out its debounce. Called on exit.
    pub fn flush(&self) {
        if let Some(w) = &self.settings_writer {
            w.flush();
        }
        if let Some(w) = &self.session_writer {
            w.flush();
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::PaneLayout;

    #[test]
    fn settings_and_session_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = AppState::load(dir.path().to_path_buf());
            let mut st = Settings::default();
            st.appearance.font_size = 16;
            s.update_settings(st);
            s.update_session(SessionState {
                layout: PaneLayout::Vertical,
                ..Default::default()
            });
            s.flush();
        }
        let s = AppState::load(dir.path().to_path_buf());
        assert_eq!(s.settings.read().appearance.font_size, 16);
        assert_eq!(s.session.read().layout, PaneLayout::Vertical);
    }

    #[test]
    fn session_is_not_restored_when_the_user_opted_out() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = AppState::load(dir.path().to_path_buf());
            let mut st = Settings::default();
            st.behavior.restore_session = false;
            s.update_settings(st);
            s.update_session(SessionState {
                layout: PaneLayout::Horizontal,
                ..Default::default()
            });
            s.flush();
        }
        let s = AppState::load(dir.path().to_path_buf());
        assert_eq!(s.session.read().layout, PaneLayout::Single);
    }
}
