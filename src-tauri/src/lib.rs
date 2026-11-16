// `unwrap` is denied in production code because a panic inside a Tauri command takes
// down the whole webview, but in tests it is the clearest way to assert success.
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! # Fulcrum — application shell
//!
//! Wires the UI to [`fx_core`]'s backends.
//!
//! The shell deliberately holds no filesystem logic of its own. It owns state, converts
//! between the synchronous backend traits and the UI's streaming expectations
//! (see [`sinks`]), and nothing more — which is what lets `fx-index` and `fx-fs` be
//! developed and tested without it.

pub mod chrome;
pub mod commands;
pub mod elevation;
pub mod persist;
pub mod sinks;
pub mod state;
pub mod switchable;

use state::AppState;
use tauri::Manager;

/// Builds and runs the application.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fulcrum=info,fx_index=info,fx_fs=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let dir = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no config directory: {e}"))?;
            let state = AppState::load(dir);
            if let Some(window) = app.get_webview_window("main") {
                chrome::apply(&window, &state.settings.read());
            }
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_dir,
            commands::search,
            commands::stat,
            commands::volumes,
            commands::index_stats,
            commands::start_scan,
            commands::start_op,
            commands::control_op,
            commands::create_dir,
            commands::rename,
            commands::open_path,
            commands::reveal,
            commands::icon,
            commands::thumbnail,
            commands::get_settings,
            commands::set_settings,
            commands::get_session,
            commands::set_session,
            commands::enable_instant_search,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Fulcrum")
        .run(|app, event| {
            // Debounced writes still pending when the user quits would otherwise be lost.
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    state.flush();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_has_default_settings() {
        let s = AppState::new();
        assert_eq!(s.settings.read().appearance.accent, fx_core::Accent::Purple);
        assert!(s.settings.read().behavior.confirm_delete);
    }

    #[test]
    fn session_round_trips_through_state() {
        let s = AppState::new();
        s.update_session(fx_core::SessionState {
            layout: fx_core::PaneLayout::Vertical,
            panes: vec![vec![], vec![]],
            ..Default::default()
        });
        assert_eq!(s.session.read().layout, fx_core::PaneLayout::Vertical);
        assert_eq!(s.session.read().panes.len(), 2);
    }
}
