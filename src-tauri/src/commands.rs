//! Tauri commands — the entire Rust↔UI surface.
//!
//! Two invariants hold across every command here:
//!
//! 1. **Nothing blocks the async runtime.** Backend calls are CPU- and IO-bound and
//!    synchronous by design, so they run under `spawn_blocking`. A directory listing on
//!    the main runtime thread would freeze the window.
//! 2. **Errors are values.** Commands return `Result<_, FxError>`, which serialises to a
//!    tagged object the UI can branch on — in particular
//!    [`fx_core::FxError::NotElevated`], which triggers the "Enable instant search" flow
//!    rather than an error toast.

use std::sync::Arc;
use std::time::Duration;

use fx_core::{
    FileEntry, FxError, IndexStats, ListOptions, ListSummary, OpControl, OpId, Progress, Query,
    QuerySummary, SearchHit, SessionState, Settings, UpdateInfo, VolumeInfo,
};
use tauri::ipc::Channel;
use tauri::State;

use crate::sinks::{ChannelSink, ThrottledProgress};
use crate::state::AppState;

/// Runs a blocking backend call off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T, FxError>
where
    F: FnOnce() -> Result<T, FxError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| FxError::Other(format!("worker thread panicked: {e}")))?
}

#[tauri::command]
pub async fn list_dir(
    state: State<'_, AppState>,
    path: String,
    options: ListOptions,
    on_batch: Channel<Vec<FileEntry>>,
) -> Result<ListSummary, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || {
        let sink = ChannelSink::new(on_batch);
        fs.list(&path, &options, &sink)
    })
    .await
}

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query: Query,
    on_batch: Channel<Vec<SearchHit>>,
) -> Result<QuerySummary, FxError> {
    let index = Arc::clone(&state.index);
    blocking(move || {
        let sink = ChannelSink::new(on_batch);
        index.query(&query, &sink)
    })
    .await
}

#[tauri::command]
pub async fn stat(state: State<'_, AppState>, path: String) -> Result<FileEntry, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.stat(&path)).await
}

#[tauri::command]
pub async fn volumes(state: State<'_, AppState>) -> Result<Vec<VolumeInfo>, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.volumes()).await
}

#[tauri::command]
pub fn index_stats(state: State<'_, AppState>) -> IndexStats {
    state.index.stats()
}

#[tauri::command]
pub async fn start_scan(
    state: State<'_, AppState>,
    on_progress: Channel<Progress>,
) -> Result<(), FxError> {
    let index = Arc::clone(&state.index);
    blocking(move || {
        let sink = ThrottledProgress::new(on_progress);
        index.scan(&sink)
    })
    .await
}

#[tauri::command]
pub async fn start_op(
    state: State<'_, AppState>,
    request: fx_core::FsOpRequest,
    on_progress: Channel<Progress>,
) -> Result<OpId, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || {
        let sink = ThrottledProgress::new(on_progress);
        fs.start_op(request, &sink)
    })
    .await
}

#[tauri::command]
pub async fn control_op(
    state: State<'_, AppState>,
    op: OpId,
    control: OpControl,
) -> Result<(), FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.control_op(op, control)).await
}

#[tauri::command]
pub async fn create_dir(state: State<'_, AppState>, path: String) -> Result<(), FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.create_dir(&path)).await
}

#[tauri::command]
pub async fn rename(
    state: State<'_, AppState>,
    path: String,
    new_name: String,
) -> Result<(), FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.rename(&path, &new_name)).await
}

#[tauri::command]
pub async fn open_path(state: State<'_, AppState>, path: String) -> Result<(), FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.open(&path)).await
}

#[tauri::command]
pub async fn reveal(state: State<'_, AppState>, path: String) -> Result<(), FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.reveal(&path)).await
}

#[tauri::command]
pub async fn icon(
    state: State<'_, AppState>,
    path: String,
    size_px: u32,
) -> Result<fx_core::Thumbnail, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.icon(&path, size_px)).await
}

#[tauri::command]
pub async fn thumbnail(
    state: State<'_, AppState>,
    path: String,
    size_px: u32,
) -> Result<fx_core::Thumbnail, FxError> {
    let fs = Arc::clone(&state.fs);
    blocking(move || fs.thumbnail(&path, size_px)).await
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().clone()
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, window: tauri::WebviewWindow, settings: Settings) {
    crate::chrome::apply(&window, &settings);
    state.update_settings(settings);
}

#[tauri::command]
pub fn get_session(state: State<'_, AppState>) -> SessionState {
    state.session.read().clone()
}

#[tauri::command]
pub fn set_session(state: State<'_, AppState>, session: SessionState) {
    state.update_session(session);
}

/// Polls until the freshly installed helper answers its pipe, or `timeout` passes.
#[cfg(windows)]
fn connect_helper(
    timeout: Duration,
    interval: Duration,
) -> Result<Arc<dyn fx_core::IndexBackend>, FxError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match fx_index::ipc::HelperClient::connect_pipe() {
            Ok(client) => return Ok(Arc::new(client)),
            Err(e) if std::time::Instant::now() >= deadline => {
                return Err(FxError::HelperUnavailable(format!(
                    "the index service did not start answering in time: {e}"
                )))
            }
            Err(_) => std::thread::sleep(interval),
        }
    }
}

#[cfg(not(windows))]
fn connect_helper(
    _timeout: Duration,
    _interval: Duration,
) -> Result<Arc<dyn fx_core::IndexBackend>, FxError> {
    Err(FxError::HelperUnavailable(
        "the index service is only available on Windows".into(),
    ))
}

/// Launches the elevated helper to install the index service.
///
/// This is the only place the app asks for elevation, and it happens once: the helper
/// registers itself as a service and the unelevated app talks to it over a named pipe
/// from then on. Declining UAC returns [`FxError::NotElevated`] and leaves the walk
/// backend in place.
#[tauri::command]
pub async fn enable_instant_search(state: State<'_, AppState>) -> Result<(), FxError> {
    let switch = Arc::clone(&state.switch);
    blocking(move || {
        crate::elevation::install_helper()?;
        // The service needs a moment to register and open its pipe.
        let helper = connect_helper(Duration::from_secs(30), Duration::from_millis(250))?;
        switch.swap(helper);
        Ok(())
    })
    .await
}

/// Looks for a newer release. `None` means this is the latest version.
///
/// Errors (offline, GitHub unreachable, a bad signature) are returned to the UI, which
/// stays quiet for the automatic startup check and reports them for a manual one.
#[tauri::command]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, FxError> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| FxError::Other(format!("updater unavailable: {e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| FxError::Other(format!("update check failed: {e}")))?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        current_version: u.current_version,
        notes: u.body,
    }))
}

/// Downloads and installs the newest release, then the installer restarts the app.
///
/// The release is checked again rather than trusted from an earlier call, and the download
/// is verified against the signing key built into the app before anything is run. Settings
/// and the session are written to disk first, because on Windows the process exits as soon
/// as the installer starts.
#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), FxError> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| FxError::Other(format!("updater unavailable: {e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| FxError::Other(format!("update check failed: {e}")))?
        .ok_or_else(|| FxError::NotFound("no update is available".into()))?;
    state.flush();
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| FxError::Other(format!("update failed: {e}")))
}
