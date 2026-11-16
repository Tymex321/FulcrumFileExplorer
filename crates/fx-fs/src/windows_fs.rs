//! Windows shell and Win32 integration. **Windows only.**
//!
//! This is where the app earns the right to call itself a replacement
//! rather than a viewer: using the real shell APIs means third-party context menus,
//! thumbnail handlers, and the actual Recycle Bin all work as users expect.
//!
//! ## What to use, and why
//!
//! | Need | API | Why not the obvious alternative |
//! |---|---|---|
//! | Delete to Recycle Bin | `IFileOperation` | `SHFileOperation` is deprecated and lacks per-item progress |
//! | Thumbnails | `IShellItemImageFactory` | Gets Explorer's cache and third-party handlers for free |
//! | Context menu | `IShellFolder` + `IContextMenu` | The only way to show real shell extensions |
//! | Volume info | `GetVolumeInformationW`, `GetDiskFreeSpaceExW` | — |
//! | Open / reveal | `ShellExecuteW`, `SHOpenFolderAndSelectItems` | — |
//!
//! ## Pitfalls
//!
//! - **COM must be initialised per thread** (`CoInitializeEx` with apartment-threaded) and
//!   these calls must not be made from a thread that never initialised it. Every entry
//!   point here calls [`ensure_com`] first, so callers on a blocking pool need not care.
//! - **Long paths:** prefix with `\\?\` to escape `MAX_PATH`. Shell APIs reject that
//!   prefix, so everything that goes to the shell passes through [`shell_path`] first.
//!   (`std::fs`, used by listing and the copy engine, adds the prefix itself.)
//! - **Attributes:** `hidden`/`system` come from the file attribute bits (see
//!   `list::attributes`), not from a leading dot as the Unix twin does.
//! - The recycle bin is per-volume; items on a network share cannot be recycled at all, so
//!   [`recycle`] reports that rather than letting the shell silently delete permanently.

// Every unsafe block in this module is a Win32/COM FFI call. The invariants are the usual
// ones (valid null-terminated wide strings that outlive the call, out-pointers to live
// locals, every handle released exactly once by a guard) and are noted where non-obvious.
#![allow(unsafe_code)]

use fx_core::{Filesystem, FxError, Result, VolumeInfo};
use std::cell::Cell;
use std::path::Path;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FileOperation, IContextMenu, IFileOperation, ILFree, IShellFolder, IShellItem,
    IShellItemImageFactory, SHBindToParent, SHCreateItemFromParsingName,
    SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW, CMF_NORMAL, CMINVOKECOMMANDINFO,
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, TrackPopupMenuEx, SW_SHOWNORMAL, TPM_RETURNCMD,
};

/// Prefix that lifts the 260-character `MAX_PATH` limit for Win32 file APIs.
pub const LONG_PATH_PREFIX: &str = r"\\?\";

// `GetDriveTypeW` results. Defined here because the `windows` crate files them under
// `System_WindowsProgramming`, a large feature this crate otherwise has no use for.
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

/// Applies the long-path prefix when needed.
///
/// Already-prefixed paths, and UNC paths (which need `\\?\UNC\`), are left alone so the
/// prefix is never doubled.
pub fn long_path(path: &str) -> String {
    if path.starts_with(LONG_PATH_PREFIX) || path.starts_with(r"\\") {
        path.to_string()
    } else {
        format!("{LONG_PATH_PREFIX}{path}")
    }
}

/// The form of a path the shell accepts: no `\\?\` prefix, backslash separators.
pub fn shell_path(path: &str) -> String {
    let p = path.replace('/', "\\");
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = p.strip_prefix(LONG_PATH_PREFIX) {
        rest.to_string()
    } else {
        p
    }
}

/// The drive root (`C:\`) a path lives on, or `None` for UNC paths.
pub fn drive_root(path: &str) -> Option<String> {
    let p = shell_path(path);
    let b = p.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        .then(|| format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn win_err(context: &str, e: windows::core::Error) -> FxError {
    // E_ACCESSDENIED and ERROR_ACCESS_DENIED as an HRESULT.
    match e.code().0 as u32 {
        0x8007_0005 => FxError::AccessDenied(format!("{context}: {}", e.message())),
        0x8007_0002 | 0x8007_0003 => FxError::NotFound(context.to_string()),
        _ => FxError::Io(format!("{context}: {}", e.message())),
    }
}

/// Initialises COM (apartment-threaded) once per thread.
///
/// Never uninitialised: these are long-lived pool threads, and tearing COM down while a
/// cached shell object might still be referenced is worse than the tiny leak. If the
/// thread already joined the multithreaded apartment, the shell calls used here work
/// there too, so `RPC_E_CHANGED_MODE` is fine.
pub fn ensure_com() -> Result<()> {
    thread_local!(static READY: Cell<bool> = const { Cell::new(false) });
    if READY.with(Cell::get) {
        return Ok(());
    }
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_ok() || hr == RPC_E_CHANGED_MODE {
        READY.with(|r| r.set(true));
        Ok(())
    } else {
        Err(FxError::Other(format!("COM initialisation failed: {hr:?}")))
    }
}

// ---- volumes ----------------------------------------------------------------------------

/// Enumerates drives with labels, capacity, and whether they can be MFT-indexed.
pub fn volumes() -> Result<Vec<VolumeInfo>> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u8 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i) as char;
        let root = format!("{letter}:\\");
        let w = wide(&root);
        let root_w = PCWSTR(w.as_ptr());

        let drive_type = unsafe { GetDriveTypeW(root_w) };
        if drive_type == DRIVE_NO_ROOT_DIR {
            continue;
        }
        let mut label = [0u16; 261];
        let mut fs_name = [0u16; 261];
        // Fails for an empty card reader or optical drive. Explorer still lists those,
        // so keep the drive with what we know rather than hiding it.
        let has_media = unsafe {
            GetVolumeInformationW(
                root_w,
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        }
        .is_ok();
        let (mut total, mut free) = (0u64, 0u64);
        if has_media {
            let _ = unsafe { GetDiskFreeSpaceExW(root_w, None, Some(&mut total), Some(&mut free)) };
        }
        out.push(volume_info(
            letter,
            drive_type,
            &from_wide(&label),
            &from_wide(&fs_name),
            total,
            free,
        ));
    }
    Ok(out)
}

/// Assembles a [`VolumeInfo`] from the raw Win32 answers. Separate so it is testable.
fn volume_info(
    letter: char,
    drive_type: u32,
    label: &str,
    fs_name: &str,
    total_bytes: u64,
    free_bytes: u64,
) -> VolumeInfo {
    let remote = drive_type == DRIVE_REMOTE;
    let filesystem = if remote {
        Filesystem::Network
    } else {
        parse_filesystem(fs_name)
    };
    let label = if label.trim().is_empty() {
        match drive_type {
            DRIVE_REMOVABLE => "USB Drive",
            DRIVE_REMOTE => "Network Drive",
            DRIVE_CDROM => "DVD Drive",
            _ => "Local Disk",
        }
        .to_string()
    } else {
        label.to_string()
    };
    VolumeInfo {
        id: letter.to_string(),
        mount: format!("{letter}:\\"),
        label,
        filesystem,
        total_bytes,
        free_bytes,
        removable: matches!(drive_type, DRIVE_REMOVABLE | DRIVE_CDROM),
        // A mapped drive can report "NTFS" for the server's disk, but there is no raw
        // volume to read the MFT from here.
        indexable: filesystem.supports_mft() && !remote,
    }
}

/// Maps a filesystem name from `GetVolumeInformationW` to [`Filesystem`].
pub fn parse_filesystem(name: &str) -> Filesystem {
    match name.to_ascii_uppercase().as_str() {
        "NTFS" => Filesystem::Ntfs,
        "REFS" => Filesystem::ReFS,
        "FAT32" | "FAT" => Filesystem::Fat32,
        "EXFAT" => Filesystem::ExFat,
        _ => Filesystem::Other,
    }
}

fn is_network_path(path: &str) -> bool {
    let p = shell_path(path);
    if p.starts_with(r"\\") {
        return true;
    }
    drive_root(&p).is_some_and(|root| {
        let w = wide(&root);
        unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == DRIVE_REMOTE }
    })
}

// ---- recycle bin ------------------------------------------------------------------------

/// Sends one item to the Recycle Bin through `IFileOperation`.
///
/// Network locations have no Recycle Bin; the shell would delete them permanently, so
/// they are refused up front. For an item too large for the bin, `FOF_WANTNUKEWARNING`
/// makes the shell ask before destroying it instead of doing so silently.
pub fn recycle(path: &Path) -> Result<()> {
    let display = path.to_string_lossy();
    if std::fs::symlink_metadata(path).is_err() {
        return Err(FxError::NotFound(display.into_owned()));
    }
    if is_network_path(&display) {
        return Err(FxError::Other(format!(
            "{display} is on a network location, which has no Recycle Bin; \
             delete it permanently instead"
        )));
    }
    ensure_com()?;
    let w = wide(&shell_path(&display));
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None)
            .map_err(|e| win_err(&display, e))?;
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)
            .map_err(|e| win_err("IFileOperation", e))?;
        op.SetOperationFlags(
            FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING,
        )
        .map_err(|e| win_err("IFileOperation flags", e))?;
        op.DeleteItem(&item, None)
            .map_err(|e| win_err(&display, e))?;
        op.PerformOperations().map_err(|e| win_err(&display, e))?;
        if op
            .GetAnyOperationsAborted()
            .map(|b| b.as_bool())
            .unwrap_or(false)
        {
            return Err(FxError::Other(format!(
                "{display} was not moved to the Recycle Bin"
            )));
        }
    }
    Ok(())
}

// ---- thumbnails and icons ---------------------------------------------------------------

fn image_factory(path: &Path) -> Result<IShellItemImageFactory> {
    ensure_com()?;
    let display = path.to_string_lossy();
    let w = wide(&shell_path(&display));
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }
        .map_err(|e| win_err(&display, e))
}

/// Content thumbnail as PNG, or `None` when no thumbnail handler covers the type (the
/// caller then falls back to the icon). Goes through Explorer's own thumbnail cache.
pub fn render_thumbnail(path: &Path, px: u32) -> Result<Option<Vec<u8>>> {
    let factory = image_factory(path)?;
    let size = SIZE {
        cx: px as i32,
        cy: px as i32,
    };
    match unsafe { factory.GetImage(size, SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY) } {
        Ok(hbm) => hbitmap_to_png(hbm).map(Some),
        Err(_) => Ok(None),
    }
}

/// The shell's icon for a path, as PNG.
pub fn render_icon(path: &Path, px: u32) -> Result<Vec<u8>> {
    let factory = image_factory(path)?;
    let size = SIZE {
        cx: px as i32,
        cy: px as i32,
    };
    let hbm = unsafe { factory.GetImage(size, SIIGBF_ICONONLY) }
        .map_err(|e| win_err(&path.to_string_lossy(), e))?;
    hbitmap_to_png(hbm)
}

/// Deletes a GDI bitmap when dropped, so every exit path releases it.
struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(self.0)) };
    }
}

fn hbitmap_to_png(hbm: HBITMAP) -> Result<Vec<u8>> {
    let bitmap = OwnedBitmap(hbm);
    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(
            HGDIOBJ::from(bitmap.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut bm as *mut BITMAP).cast()),
        )
    };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return Err(FxError::Other("shell returned an empty bitmap".into()));
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());

    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative height asks for top-down rows, which is what PNG wants.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    let lines = unsafe {
        let dc = CreateCompatibleDC(None);
        let n = GetDIBits(
            dc,
            bitmap.0,
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        n
    };
    if lines == 0 {
        return Err(FxError::Other("could not read thumbnail pixels".into()));
    }
    bgra_premultiplied_to_rgba(&mut pixels);
    encode_png(w as u32, h as u32, &pixels)
}

/// Shell bitmaps are BGRA with premultiplied alpha — or, for some handlers, BGRX with an
/// alpha channel of all zeros, which must be read as opaque rather than invisible.
pub fn bgra_premultiplied_to_rgba(px: &mut [u8]) {
    let has_alpha = px.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        if !has_alpha {
            p[3] = 255;
        } else if p[3] != 0 && p[3] != 255 {
            let a = u16::from(p[3]);
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| FxError::Other(format!("png: {e}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| FxError::Other(format!("png: {e}")))?;
        writer
            .finish()
            .map_err(|e| FxError::Other(format!("png: {e}")))?;
    }
    Ok(out)
}

// ---- open, reveal, context menu ---------------------------------------------------------

/// Opens with the default handler, exactly as a double-click in Explorer would.
pub fn open(path: &str) -> Result<()> {
    if std::fs::symlink_metadata(path).is_err() {
        return Err(FxError::NotFound(path.into()));
    }
    ensure_com()?;
    let file = wide(&shell_path(path));
    let verb = wide("open");
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute's documented failure signal: a return value of 32 or less.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(match code {
            2 | 3 => FxError::NotFound(path.into()),
            5 => FxError::AccessDenied(path.into()),
            31 => FxError::Other(format!("no application is associated with {path}")),
            _ => FxError::Io(format!("could not open {path} (code {code})")),
        })
    }
}

/// An absolute PIDL, freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &str) -> Result<Self> {
        let w = wide(&shell_path(path));
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| win_err(path, e))?;
        Ok(Self(pidl))
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ILFree(Some(self.0)) };
        }
    }
}

/// Opens the containing folder in Explorer with the item selected.
pub fn reveal(path: &str) -> Result<()> {
    ensure_com()?;
    let pidl = Pidl::parse(path)?;
    // With no child list, the shell opens the item's parent and selects the item.
    unsafe { SHOpenFolderAndSelectItems(pidl.0, None, 0) }.map_err(|e| win_err(path, e))
}

/// Shows the real Explorer context menu for `paths` at screen coordinates `(x, y)` and
/// runs the chosen command. Returns whether a command was chosen.
///
/// Not part of [`fx_core::FsBackend`] (it needs a window handle), so the shell calls it
/// directly on Windows. `paths` must share one parent folder, as any Explorer selection
/// does. `hwnd` must be a foreground window owned by the calling thread.
///
/// Limitation: cascading menus built lazily by `IContextMenu2/3` handlers ("Send to",
/// "Open with") need the window procedure to forward `WM_INITMENUPOPUP` and friends to
/// `HandleMenuMsg`; without that hook they appear empty. Top-level verbs, including
/// third-party ones, work.
pub fn show_context_menu(hwnd: isize, paths: &[String], x: i32, y: i32) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    ensure_com()?;
    let pidls = paths
        .iter()
        .map(|p| Pidl::parse(p))
        .collect::<Result<Vec<_>>>()?;
    let owner = HWND(hwnd as *mut core::ffi::c_void);

    unsafe {
        let mut parent: Option<IShellFolder> = None;
        let mut children: Vec<*const ITEMIDLIST> = Vec::with_capacity(pidls.len());
        for pidl in &pidls {
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            // `child` points inside `pidl`, which outlives this function's use of it.
            let folder: IShellFolder =
                SHBindToParent(pidl.0, Some(&mut child)).map_err(|e| win_err("context menu", e))?;
            parent.get_or_insert(folder);
            children.push(child);
        }
        let Some(parent) = parent else {
            return Ok(false);
        };
        let menu: IContextMenu = parent
            .GetUIObjectOf(owner, &children, None)
            .map_err(|e| win_err("context menu", e))?;

        let hmenu = CreatePopupMenu().map_err(|e| win_err("context menu", e))?;
        const FIRST_ID: u32 = 1;
        let shown = menu
            .QueryContextMenu(hmenu, 0, FIRST_ID, 0x7FFF, CMF_NORMAL)
            .ok()
            .map(|()| TrackPopupMenuEx(hmenu, TPM_RETURNCMD.0, x, y, owner, None).0);
        let _ = DestroyMenu(hmenu);
        let chosen = shown.map_err(|e| win_err("context menu", e))?;
        if chosen <= 0 {
            return Ok(false);
        }

        let info = CMINVOKECOMMANDINFO {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
            hwnd: owner,
            // MAKEINTRESOURCE: the verb is the command's offset from FIRST_ID.
            lpVerb: PCSTR((chosen as u32 - FIRST_ID) as usize as *const u8),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        menu.InvokeCommand(&info)
            .map_err(|e| win_err("context menu", e))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_path_prefix_is_applied_once() {
        assert_eq!(long_path(r"C:\data\file.txt"), r"\\?\C:\data\file.txt");
        // Must not double up.
        assert_eq!(long_path(r"\\?\C:\data"), r"\\?\C:\data");
        // UNC paths need a different form, so leave them untouched here.
        assert_eq!(long_path(r"\\server\share"), r"\\server\share");
    }

    #[test]
    fn shell_paths_never_carry_the_long_prefix() {
        assert_eq!(shell_path(r"\\?\C:\data\f.txt"), r"C:\data\f.txt");
        assert_eq!(shell_path(r"\\?\UNC\server\share\x"), r"\\server\share\x");
        assert_eq!(shell_path("C:/mixed/seps"), r"C:\mixed\seps");
        assert_eq!(shell_path(r"D:\plain"), r"D:\plain");
    }

    #[test]
    fn drive_roots() {
        assert_eq!(drive_root(r"c:\x\y").as_deref(), Some(r"C:\"));
        assert_eq!(drive_root(r"\\?\D:\x").as_deref(), Some(r"D:\"));
        assert_eq!(drive_root(r"\\server\share"), None);
    }

    #[test]
    fn only_ntfs_supports_the_mft_fast_path() {
        assert_eq!(parse_filesystem("NTFS"), Filesystem::Ntfs);
        assert!(parse_filesystem("NTFS").supports_mft());
        assert!(!parse_filesystem("exFAT").supports_mft());
        assert!(!parse_filesystem("ReFS").supports_mft());
        assert_eq!(parse_filesystem("something else"), Filesystem::Other);
    }

    #[test]
    fn volume_info_labels_and_indexability() {
        let c = volume_info('C', 3, "", "NTFS", 100, 40);
        assert_eq!((c.id.as_str(), c.mount.as_str()), ("C", r"C:\"));
        assert_eq!(c.label, "Local Disk");
        assert!(c.indexable && !c.removable);

        let usb = volume_info('E', DRIVE_REMOVABLE, "STICK", "exFAT", 1, 1);
        assert_eq!(usb.label, "STICK");
        assert!(usb.removable && !usb.indexable);

        // A mapped share reporting NTFS is still not indexable.
        let net = volume_info('Z', DRIVE_REMOTE, "", "NTFS", 1, 1);
        assert_eq!(net.filesystem, Filesystem::Network);
        assert!(!net.indexable);
        assert_eq!(net.label, "Network Drive");
    }

    #[test]
    fn pixel_conversion_unpremultiplies_and_fixes_missing_alpha() {
        // BGRA, half-transparent pure red premultiplied: B=0 G=0 R=128 A=128.
        let mut px = vec![0, 0, 128, 128];
        bgra_premultiplied_to_rgba(&mut px);
        assert_eq!(px, [255, 0, 0, 128]);

        // BGRX with zero alpha everywhere: opaque.
        let mut px = vec![10, 20, 30, 0, 40, 50, 60, 0];
        bgra_premultiplied_to_rgba(&mut px);
        assert_eq!(px, [30, 20, 10, 255, 60, 50, 40, 255]);
    }

    #[test]
    fn png_round_trip_header() {
        let png = encode_png(2, 3, &[0u8; 2 * 3 * 4]).unwrap();
        assert_eq!(crate::thumbs::png_dimensions(&png), Some((2, 3)));
    }

    #[test]
    fn volumes_include_the_system_drive() {
        let vols = volumes().unwrap();
        assert!(vols.iter().any(|v| v.mount == r"C:\" && v.total_bytes > 0));
    }

    #[test]
    fn icons_render_for_a_real_file() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let png = render_icon(&f, 32).unwrap();
        assert!(crate::thumbs::png_dimensions(&png).is_some());
    }
}
