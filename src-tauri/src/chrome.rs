//! Window chrome: translucency.
//!
//! Minimise / maximise / close are driven from the frameless title bar in the UI through
//! the permissions in `capabilities/default.json`; Rust's part is the backdrop material.
//!
//! The acrylic backdrop is expensive: while the window moves or resizes, Windows re-blurs
//! what is behind it on every frame, which makes dragging visibly laggy. It is also only
//! ever visible through a transparent window. So it is applied only when the window is
//! configured transparent and the user has turned it on; otherwise it would cost frames
//! behind an opaque page and show nothing.

use fx_core::Settings;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{Manager, Runtime, WebviewWindow};

/// Whether the window was created transparent (`app.windows[].transparent` in
/// `tauri.conf.json`), the precondition for any backdrop to be visible.
fn is_transparent<R: Runtime>(window: &WebviewWindow<R>) -> bool {
    let label = window.label();
    window
        .app_handle()
        .config()
        .app
        .windows
        .iter()
        .any(|w| w.label == label && w.transparent)
}

/// Applies or clears the acrylic backdrop. Failure is silent by design: older Windows
/// builds and other platforms simply keep the opaque `#0B0A10` window.
pub fn apply<R: Runtime>(window: &WebviewWindow<R>, settings: &Settings) {
    let result = if is_transparent(window) && settings.appearance.acrylic {
        window.set_effects(EffectsBuilder::new().effect(Effect::Acrylic).build())
    } else {
        window.set_effects(None)
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "window effect unsupported; staying opaque");
    }
}
