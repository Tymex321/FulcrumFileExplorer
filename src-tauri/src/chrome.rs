//! Window chrome: translucency.
//!
//! Minimise / maximise / close are driven from the frameless title bar in the UI through
//! the permissions in `capabilities/default.json`; Rust's part is the backdrop material.

use fx_core::Settings;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{Runtime, WebviewWindow};

/// Applies or clears the acrylic backdrop. Failure is silent by design: older Windows
/// builds and other platforms simply keep the opaque `#0B0A10` window.
pub fn apply<R: Runtime>(window: &WebviewWindow<R>, settings: &Settings) {
    let result = if settings.appearance.acrylic {
        window.set_effects(EffectsBuilder::new().effect(Effect::Acrylic).build())
    } else {
        window.set_effects(None)
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "window effect unsupported; staying opaque");
    }
}
