//! Crash-safe persistence for settings and session.
//!
//! Two rules: a write never truncates the live file (temp file + rename, so a crash leaves
//! either the old or the new content, never half of one), and writes are debounced so
//! dragging a pane divider does not hit the disk sixty times a second.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

/// How long a value must sit unchanged before it is written.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

/// Atomically replaces `path` with `bytes`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        // Without this the rename can land before the data does after a power cut.
        f.sync_all()?;
    }
    // `rename` replaces an existing file on both Unix and Windows (MoveFileExW with
    // REPLACE_EXISTING), which is what makes this atomic.
    std::fs::rename(&tmp, path)
}

/// Loads `path`, returning `None` when it is absent or unreadable.
///
/// A corrupt file is moved aside to `<name>.corrupt` rather than deleted, so a bad write
/// never costs the user their settings *and* never makes the app fail to start.
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "corrupt state file; ignoring");
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            let _ = std::fs::rename(path, PathBuf::from(aside));
            None
        }
    }
}

enum Msg<T> {
    Save(T),
    Flush(Sender<()>),
}

/// Writes the most recent value after [`DEBOUNCE`] of quiet.
pub struct DebouncedWriter<T: Serialize + Send + 'static> {
    tx: Sender<Msg<T>>,
}

impl<T: Serialize + Send + 'static> std::fmt::Debug for DebouncedWriter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DebouncedWriter")
    }
}

impl<T: Serialize + Send + 'static> DebouncedWriter<T> {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<Msg<T>>();
        let spawned = std::thread::Builder::new()
            .name("fx-persist".into())
            .spawn(move || {
                let write = |v: &T| match serde_json::to_vec_pretty(v) {
                    Ok(bytes) => {
                        if let Err(e) = write_atomic(&path, &bytes) {
                            tracing::error!(path = %path.display(), error = %e, "persist failed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "serialise failed"),
                };
                let mut pending: Option<T> = None;
                loop {
                    let msg = if pending.is_some() {
                        rx.recv_timeout(delay)
                    } else {
                        rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match msg {
                        Ok(Msg::Save(v)) => pending = Some(v),
                        Ok(Msg::Flush(ack)) => {
                            if let Some(v) = pending.take() {
                                write(&v);
                            }
                            let _ = ack.send(());
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if let Some(v) = pending.take() {
                                write(&v);
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            if let Some(v) = pending.take() {
                                write(&v);
                            }
                            return;
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "could not start persistence thread");
        }
        Self { tx }
    }

    pub fn save(&self, value: T) {
        let _ = self.tx.send(Msg::Save(value));
    }

    /// Blocks until anything pending is on disk. Called on exit.
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::Settings;

    #[test]
    fn atomic_write_replaces_existing_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!dir.path().join("a.json.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_moved_aside_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, b"{ not json").unwrap();
        assert!(load::<Settings>(&p).is_none());
        assert!(dir.path().join("settings.json.corrupt").exists());
    }

    #[test]
    fn missing_file_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load::<Settings>(&dir.path().join("nope.json")).is_none());
    }

    #[test]
    fn debounce_coalesces_to_latest_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        let w = DebouncedWriter::<Settings>::new(p.clone(), Duration::from_millis(50));
        let mut s = Settings::default();
        for size in 10..20 {
            s.appearance.font_size = size;
            w.save(s.clone());
        }
        w.flush();
        let back: Settings = load(&p).unwrap();
        assert_eq!(back.appearance.font_size, 19);
    }

    #[test]
    fn dropping_the_writer_flushes_pending_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        {
            let w = DebouncedWriter::<Settings>::new(p.clone(), Duration::from_secs(60));
            let mut s = Settings::default();
            s.appearance.font_size = 17;
            w.save(s);
        }
        // The worker finishes asynchronously after the sender drops.
        for _ in 0..50 {
            if p.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(load::<Settings>(&p).unwrap().appearance.font_size, 17);
    }
}
