//! macOS implementations. **Development only — never ships.**
//!
//! This module's whole purpose is to let the app run end to end on the
//! development machine so that UI and operation logic can be exercised without Windows.
//! Keep it simple; correctness parity with [`crate::windows_fs`] is not a goal beyond what
//! makes development realistic.

use fx_core::{Filesystem, FxError, Result, VolumeInfo};
use std::path::Path;

/// Enumerates mounted volumes: `/` plus anything under `/Volumes`.
pub fn volumes() -> Result<Vec<VolumeInfo>> {
    let mut mounts = vec![("/".to_string(), "Macintosh HD".to_string(), false)];
    if let Ok(read) = std::fs::read_dir("/Volumes") {
        for item in read.flatten() {
            // `/Volumes/Macintosh HD` is a symlink back to `/`; listing it twice would
            // show the boot disk twice in the sidebar.
            if item.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                continue;
            }
            mounts.push((
                item.path().to_string_lossy().into_owned(),
                item.file_name().to_string_lossy().into_owned(),
                true,
            ));
        }
    }

    let paths: Vec<&str> = mounts.iter().map(|(m, _, _)| m.as_str()).collect();
    let capacity = disk_capacity(&paths);
    Ok(mounts
        .into_iter()
        .map(|(mount, label, removable)| {
            let (total_bytes, free_bytes) = capacity.get(&mount).copied().unwrap_or((0, 0));
            VolumeInfo {
                id: mount.clone(),
                mount,
                label,
                filesystem: Filesystem::Apfs,
                total_bytes,
                free_bytes,
                removable,
                // No MFT here, so search always runs on the walk backend during development.
                indexable: false,
            }
        })
        .collect())
}

/// Total and free bytes per mount, from one `df` call. `std` has no `statvfs`, and this
/// twin is not worth a libc dependency; on failure the sidebar just shows no bar.
fn disk_capacity(mounts: &[&str]) -> std::collections::HashMap<String, (u64, u64)> {
    let output = std::process::Command::new("df")
        .arg("-kP")
        .args(mounts)
        .output();
    let Ok(output) = output else {
        return Default::default();
    };
    parse_df(&String::from_utf8_lossy(&output.stdout))
}

/// Parses POSIX `df -kP` output. The mount point is the last column and may contain
/// spaces, so columns are taken from the left and the remainder is the mount.
fn parse_df(text: &str) -> std::collections::HashMap<String, (u64, u64)> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines().skip(1) {
        let mut rest = line;
        let mut cols = Vec::with_capacity(5);
        for _ in 0..5 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            cols.push(&rest[..end]);
            rest = &rest[end..];
        }
        let mount = rest.trim();
        let kib = |s: &str| s.parse::<u64>().ok().map(|k| k * 1024);
        if let (Some(total), Some(free)) = (kib(cols[1]), kib(cols[3])) {
            if !mount.is_empty() {
                out.insert(mount.to_string(), (total, free));
            }
        }
    }
    out
}

/// JXA script rendering an `NSWorkspace` icon to PNG at an exact pixel size. Compiled by
/// `osascript` at run time, so no Objective-C toolchain or crate is needed.
const ICON_SCRIPT: &str = r#"
ObjC.import('AppKit');
function run(argv) {
  const mode = argv[0], arg = argv[1], n = parseInt(argv[2]), out = argv[3];
  const ws = $.NSWorkspace.sharedWorkspace;
  const img = mode === 'type' ? ws.iconForFileType(arg) : ws.iconForFile(arg);
  const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(null, n, n, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
  $.NSGraphicsContext.saveGraphicsState;
  $.NSGraphicsContext.currentContext = $.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep);
  img.drawInRectFromRectOperationFraction($.NSMakeRect(0, 0, n, n), $.NSZeroRect, $.NSCompositingOperationCopy, 1.0);
  $.NSGraphicsContext.restoreGraphicsState;
  const data = rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $());
  if (!data.writeToFileAtomically(out, true)) throw new Error('write failed');
}
"#;

/// File-type icon as PNG. Ordinary files resolve by extension (the caller caches per
/// extension); folders and self-describing types resolve by path.
pub fn render_icon(path: &Path, is_dir: bool, px: u32) -> Result<Vec<u8>> {
    let scratch = Scratch::new()?;
    let out = scratch.0.join("icon.png");
    let ext = fx_core::FileEntry::split_ext(&path.to_string_lossy()).1;
    let by_type = !is_dir && ext.as_deref().is_some_and(|e| e != "app");
    let (mode, arg) = if by_type {
        ("type", ext.unwrap_or_default())
    } else {
        ("file", path.to_string_lossy().into_owned())
    };
    let status = std::process::Command::new("osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            ICON_SCRIPT,
            mode,
            &arg,
            &px.to_string(),
        ])
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| FxError::Io(e.to_string()))?;
    if !status.success() {
        return Err(FxError::Io(format!(
            "icon rendering failed for {mode} {arg}"
        )));
    }
    std::fs::read(&out).map_err(Into::into)
}

/// QuickLook content thumbnail as PNG, or `None` when QuickLook has no generator for the
/// file (it then writes nothing).
pub fn render_thumbnail(path: &Path, px: u32) -> Result<Option<Vec<u8>>> {
    let scratch = Scratch::new()?;
    let status = std::process::Command::new("qlmanage")
        .args(["-t", "-s", &px.to_string(), "-o"])
        .arg(&scratch.0)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| FxError::Io(format!("qlmanage unavailable: {e}")))?;
    if !status.success() {
        return Ok(None);
    }
    // qlmanage names its output `<file name>.png`.
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    match std::fs::read(scratch.0.join(format!("{name}.png"))) {
        Ok(png) => Ok(Some(png)),
        Err(_) => Ok(None),
    }
}

/// A private, self-deleting temp directory, so concurrent renders never collide.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "fulcrum-render-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Moves to the Trash. Uses `NSFileManager` rather than the crate's default Finder
/// AppleScript path, which plays a sound and takes ~1s per item.
pub fn trash(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_err() {
        return Err(FxError::NotFound(path.display().to_string()));
    }
    #[cfg(target_os = "macos")]
    let ctx = {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx
    };
    #[cfg(not(target_os = "macos"))]
    let ctx = trash::TrashContext::default();
    ctx.delete(path).map_err(|e| FxError::Io(e.to_string()))
}

pub fn open(path: &str) -> Result<()> {
    spawn("open", &[path])
}

/// Reveals in Finder, the macOS twin of Explorer's "show in folder".
pub fn reveal(path: &str) -> Result<()> {
    spawn("open", &["-R", path])
}

fn spawn(program: &str, args: &[&str]) -> Result<()> {
    if std::fs::symlink_metadata(args.last().copied().unwrap_or_default()).is_err() {
        return Err(FxError::NotFound(
            args.last().copied().unwrap_or_default().into(),
        ));
    }
    let mut child = std::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| FxError::Io(e.to_string()))?;
    // Reap off-thread so the caller returns at once and no zombie is left behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_volume_is_present_and_marked_unindexable() {
        let vols = volumes().unwrap();
        let root = vols.iter().find(|v| v.mount == "/").expect("root volume");
        assert!(!root.indexable, "APFS has no MFT fast path");
        assert_eq!(root.filesystem, Filesystem::Apfs);
        assert!(root.total_bytes > 0 && root.free_bytes <= root.total_bytes);
        // The boot-disk alias under /Volumes is not listed a second time.
        assert_eq!(vols.iter().filter(|v| v.label == "Macintosh HD").count(), 1);
    }

    #[test]
    fn df_output_parses_mounts_with_spaces() {
        let text = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    /dev/disk3s1s1 971350180 10000 500000 3% /\n\
                    /dev/disk4s1 1000 400 600 40% /Volumes/My Drive\n\
                    garbage line\n";
        let m = parse_df(text);
        assert_eq!(m["/"], (971350180 * 1024, 500000 * 1024));
        assert_eq!(m["/Volumes/My Drive"], (1000 * 1024, 600 * 1024));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn opening_a_missing_path_is_not_found() {
        assert!(matches!(
            open("/definitely/not/here"),
            Err(FxError::NotFound(_))
        ));
    }

    #[test]
    fn trashing_a_missing_path_is_not_found() {
        assert!(matches!(
            trash(Path::new("/definitely/not/here")),
            Err(FxError::NotFound(_))
        ));
    }
}
