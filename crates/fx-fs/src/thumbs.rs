//! Thumbnails and icons.
//!
//! Two rules keep this from wrecking scroll performance:
//!
//! 1. **Never block a listing.** Rows paint with a type icon first; real thumbnails arrive
//!    later and replace them. Nothing in [`crate::list`] calls into this module.
//! 2. **Cache on disk, keyed by `path + mtime + size`.** Regenerating a thumbnail for an
//!    unchanged file is pure waste, and the mtime+size key invalidates correctly without
//!    hashing file contents.
//!
//! Icons are cached **in memory per extension** (plus per file for the few types whose
//! icon is embedded in the file itself, like `.exe`), so a 10,000-row listing of JPEGs
//! resolves one icon, not ten thousand.
//!
//! Platform sources: `IShellItemImageFactory` on Windows (which also gives Explorer's own
//! thumbnails for free, including third-party handlers), QuickLook and `NSWorkspace` on
//! macOS. Platform renderers return PNG bytes; this module owns caching and transport.

use base64::Engine as _;
use fx_core::{FxError, Result, Thumbnail};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Icon size for list rows. Larger sizes are for grid and tile views.
pub const ROW_ICON_PX: u32 = 16;

/// Upper bound on requested thumbnail size, to keep IPC payloads sane.
pub const MAX_THUMB_PX: u32 = 512;

/// Below this the result is unrecognisable; clamp rather than render noise.
const MIN_PX: u32 = 8;

/// Cache key for a thumbnail. `mtime` and `size` make this self-invalidating.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub path: String,
    pub mtime: i64,
    pub size: u64,
    pub px: u32,
}

impl ThumbKey {
    /// Stable filename for the on-disk cache entry.
    ///
    /// SHA-256 over every field, truncated to 128 bits: collision-free in practice, and
    /// stable across Rust releases (unlike `DefaultHasher`) so the cache survives
    /// upgrades. The path is length-prefixed so field boundaries cannot alias.
    pub fn cache_name(&self) -> String {
        let mut h = Sha256::new();
        h.update((self.path.len() as u64).to_le_bytes());
        h.update(self.path.as_bytes());
        h.update(self.mtime.to_le_bytes());
        h.update(self.size.to_le_bytes());
        h.update(self.px.to_le_bytes());
        let digest = h.finalize();
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}.png")
    }
}

/// Where cached thumbnails live: the platform cache directory, falling back to temp.
pub fn cache_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Fulcrum"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches/Fulcrum"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("fulcrum"))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("fulcrum"))
        .join("thumbs")
}

/// Content thumbnail, falling back to a type icon when no preview exists.
pub fn thumbnail(path: &str, size_px: u32) -> Result<Thumbnail> {
    thumbnail_in(&cache_dir(), path, size_px)
}

fn thumbnail_in(cache: &Path, path: &str, size_px: u32) -> Result<Thumbnail> {
    let px = size_px.clamp(MIN_PX, MAX_THUMB_PX);
    let meta = std::fs::metadata(path)?;
    let ext = fx_core::FileEntry::split_ext(path).1;

    // Windows shell handlers cover far more than the media list (Office documents,
    // fonts, 3D models), so ask the shell for everything there. Elsewhere, skip the
    // ~100 ms QuickLook spawn for types that cannot have a preview.
    if meta.is_dir() || (!cfg!(windows) && !has_preview(ext.as_deref())) {
        return icon(path, px);
    }

    let key = ThumbKey {
        path: path.to_string(),
        mtime: crate::list::system_time_millis(meta.modified().ok()),
        size: meta.len(),
        px,
    };
    let cached = cache.join(key.cache_name());
    if let Ok(png) = std::fs::read(&cached) {
        if let Some(t) = encode(&png, false) {
            return Ok(t);
        }
    }

    match render_thumbnail(Path::new(path), px) {
        Ok(Some(png)) => {
            store(cache, &cached, &png);
            encode(&png, false)
                .ok_or_else(|| FxError::Other("thumbnailer produced an invalid PNG".into()))
        }
        // No handler for this file: the type icon is the honest answer.
        Ok(None) => icon(path, px),
        Err(e) => {
            tracing::debug!(path, error = %e, "thumbnail failed; using type icon");
            icon(path, px)
        }
    }
}

/// Writes a cache entry atomically, so a concurrent reader never sees half a PNG. Cache
/// failures are deliberately ignored: they cost a regeneration, never a thumbnail.
fn store(cache: &Path, target: &Path, png: &[u8]) {
    if std::fs::create_dir_all(cache).is_err() {
        return;
    }
    let tmp = target.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, png).is_ok() && std::fs::rename(&tmp, target).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// File-type icon. Cheaper than [`thumbnail`]; used for every list row.
pub fn icon(path: &str, size_px: u32) -> Result<Thumbnail> {
    let px = size_px.clamp(MIN_PX, MAX_THUMB_PX);
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    let key = (icon_key(path, is_dir), px);

    static CACHE: OnceLock<Mutex<HashMap<(String, u32), Thumbnail>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().get(&key) {
        return Ok(t.clone());
    }
    // Rendered outside the lock: a slow shell call must not stall every other row.
    let png = render_icon(Path::new(path), is_dir, px)?;
    let t = encode(&png, true)
        .ok_or_else(|| FxError::Other("icon renderer produced an invalid PNG".into()))?;
    cache.lock().insert(key, t.clone());
    Ok(t)
}

/// What an icon is cached under: the extension for ordinary files, the path itself for
/// types whose icon lives inside the file.
pub fn icon_key(path: &str, is_dir: bool) -> String {
    if is_dir {
        return "<dir>".into();
    }
    match fx_core::FileEntry::split_ext(path).1.as_deref() {
        Some(ext @ ("exe" | "ico" | "lnk" | "url" | "scr" | "cur" | "ani" | "msc" | "app")) => {
            format!("{ext}:{path}")
        }
        Some(ext) => format!(".{ext}"),
        None => "<none>".into(),
    }
}

fn encode(png: &[u8], is_fallback: bool) -> Option<Thumbnail> {
    let (width, height) = png_dimensions(png)?;
    Some(Thumbnail {
        data_base64: base64::engine::general_purpose::STANDARD.encode(png),
        width,
        height,
        is_fallback,
    })
}

/// Reads width and height from a PNG's IHDR chunk, rejecting anything that is not one.
pub fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if png.len() < 24 || png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Some((be(&png[16..20]), be(&png[20..24])))
}

#[cfg(windows)]
fn render_thumbnail(path: &Path, px: u32) -> Result<Option<Vec<u8>>> {
    crate::windows_fs::render_thumbnail(path, px)
}

#[cfg(windows)]
fn render_icon(path: &Path, _is_dir: bool, px: u32) -> Result<Vec<u8>> {
    crate::windows_fs::render_icon(path, px)
}

#[cfg(not(windows))]
fn render_thumbnail(path: &Path, px: u32) -> Result<Option<Vec<u8>>> {
    crate::unix_fs::render_thumbnail(path, px)
}

#[cfg(not(windows))]
fn render_icon(path: &Path, is_dir: bool, px: u32) -> Result<Vec<u8>> {
    crate::unix_fs::render_icon(path, is_dir, px)
}

/// Whether a file type is worth generating a content thumbnail for. Checked before
/// dispatching work, so unsupported types cost nothing.
pub fn has_preview(ext: Option<&str>) -> bool {
    matches!(
        ext,
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "bmp"
                | "webp"
                | "tiff"
                | "ico"
                | "svg"
                | "avif"
                | "heic"
                | "pdf"
                | "mp4"
                | "mov"
                | "mkv"
                | "avi"
                | "webm"
                | "psd"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_when_the_file_changes() {
        let base = ThumbKey {
            path: "/a/b.png".into(),
            mtime: 1,
            size: 10,
            px: 64,
        };
        let touched = ThumbKey {
            mtime: 2,
            ..base.clone()
        };
        let resized = ThumbKey {
            size: 11,
            ..base.clone()
        };
        let bigger = ThumbKey {
            px: 128,
            ..base.clone()
        };

        assert_ne!(base.cache_name(), touched.cache_name());
        assert_ne!(base.cache_name(), resized.cache_name());
        assert_ne!(base.cache_name(), bigger.cache_name());
        // Identical inputs must reuse the cached entry.
        assert_eq!(base.cache_name(), base.cache_name());
    }

    #[test]
    fn different_paths_get_different_keys() {
        let a = ThumbKey {
            path: "/a/one.png".into(),
            mtime: 1,
            size: 1,
            px: 64,
        };
        let b = ThumbKey {
            path: "/a/two.png".into(),
            ..a
        };
        assert_ne!(a.cache_name(), b.cache_name());
    }

    #[test]
    fn cache_name_is_a_fixed_length_filename() {
        let k = ThumbKey {
            path: "C:\\some\\very\\long\\path\\with spaces\\and ünïcode.jpg".into(),
            mtime: -5,
            size: u64::MAX,
            px: 512,
        };
        let name = k.cache_name();
        assert_eq!(name.len(), 32 + ".png".len());
        assert!(name[..32].chars().all(|c| c.is_ascii_hexdigit()));
        // Pinned, so an accidental change to the key scheme (which would orphan every
        // user's cache) shows up as a test failure.
        let pinned = ThumbKey {
            path: "/a".into(),
            mtime: 0,
            size: 0,
            px: 16,
        };
        assert_eq!(pinned.cache_name(), "17e2fcce149dbdbc8ae60e496a3ef10f.png");
    }

    #[test]
    fn preview_support_is_limited_to_media_types() {
        assert!(has_preview(Some("png")));
        assert!(has_preview(Some("pdf")));
        assert!(has_preview(Some("mp4")));
        assert!(!has_preview(Some("rs")));
        assert!(!has_preview(Some("exe")));
        assert!(!has_preview(None));
    }

    #[test]
    fn icons_are_keyed_per_extension_except_self_describing_types() {
        assert_eq!(icon_key("/x/a.JPG", false), ".jpg");
        assert_eq!(icon_key("/y/b.jpg", false), ".jpg");
        assert_eq!(icon_key("/y/Makefile", false), "<none>");
        assert_eq!(icon_key("/y/photos.d", true), "<dir>");
        // Each executable has its own embedded icon.
        assert_ne!(
            icon_key(r"C:\a\setup.exe", false),
            icon_key(r"C:\b\setup.exe", false)
        );
    }

    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn png_header_parsing() {
        assert_eq!(png_dimensions(&tiny_png(64, 48)), Some((64, 48)));
        assert_eq!(png_dimensions(b"GIF89a not a png at all...."), None);
        assert_eq!(png_dimensions(&tiny_png(1, 1)[..20]), None);
    }

    #[test]
    fn encode_reports_dimensions_and_base64() {
        let t = encode(&tiny_png(3, 4), true).unwrap();
        assert_eq!((t.width, t.height, t.is_fallback), (3, 4, true));
        let back = base64::engine::general_purpose::STANDARD
            .decode(&t.data_base64)
            .unwrap();
        assert_eq!(back, tiny_png(3, 4));
    }

    #[test]
    fn cached_thumbnails_are_served_without_rendering() {
        let cache = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let img = d.path().join("pic.png");
        std::fs::write(&img, b"not really an image").unwrap();
        let meta = std::fs::metadata(&img).unwrap();
        let key = ThumbKey {
            path: img.to_str().unwrap().to_string(),
            mtime: crate::list::system_time_millis(meta.modified().ok()),
            size: meta.len(),
            px: 64,
        };
        std::fs::write(cache.path().join(key.cache_name()), tiny_png(64, 40)).unwrap();

        let t = thumbnail_in(cache.path(), img.to_str().unwrap(), 64).unwrap();
        assert_eq!((t.width, t.height, t.is_fallback), (64, 40, false));
    }

    #[test]
    fn missing_file_is_not_found() {
        let cache = tempfile::tempdir().unwrap();
        assert!(matches!(
            thumbnail_in(cache.path(), "/definitely/not/here.png", 64),
            Err(FxError::NotFound(_))
        ));
    }

    #[test]
    fn store_is_atomic_and_leaves_no_temp_files() {
        let cache = tempfile::tempdir().unwrap();
        let sub = cache.path().join("nested");
        let target = sub.join("x.png");
        store(&sub, &target, &tiny_png(1, 1));
        assert_eq!(std::fs::read(&target).unwrap(), tiny_png(1, 1));
        assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 1);
    }

    /// Exercises the real QuickLook/NSWorkspace path. Needs a GUI session, so it is
    /// opt-in: `cargo test -p fx-fs -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_renders_real_icons_and_thumbnails() {
        let d = tempfile::tempdir().unwrap();
        let txt = d.path().join("a.txt");
        std::fs::write(&txt, b"hello").unwrap();
        let i = icon(txt.to_str().unwrap(), 32).unwrap();
        assert!(i.is_fallback && i.width == 32);
        let t = thumbnail(txt.to_str().unwrap(), 64).unwrap();
        assert!(t.is_fallback, "txt has no preview, so the icon comes back");
    }
}
