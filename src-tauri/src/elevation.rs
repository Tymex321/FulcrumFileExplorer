//! The one place the app asks for administrator rights.
//!
//! The app itself stays `asInvoker`. Only `fx-helper.exe install` is launched through the
//! `runas` verb, once, to register the index service; from then on the unelevated app
//! talks to that service over a pipe.

use std::path::PathBuf;

use fx_core::FxError;

/// Where `fx-helper.exe` lives: beside the app binary in both dev and installed layouts.
pub fn helper_path() -> Result<PathBuf, FxError> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| FxError::Other("app has no parent directory".into()))?;
    let name = if cfg!(windows) {
        "fx-helper.exe"
    } else {
        "fx-helper"
    };
    Ok(dir.join(name))
}

/// Runs `fx-helper install` elevated and waits for the UAC decision.
///
/// Returns [`FxError::NotElevated`] when the user declines, so the UI can keep the
/// "Enable instant search" affordance instead of showing an error.
#[cfg(windows)]
pub fn install_helper() -> Result<(), FxError> {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;

    let helper = helper_path()?;
    if !helper.exists() {
        return Err(FxError::HelperUnavailable(format!(
            "{} not found",
            helper.display()
        )));
    }
    let file = HSTRING::from(helper.as_os_str());
    // SW_HIDE (0): the installer has no UI of its own.
    // SAFETY: all string arguments outlive the call; the returned HINSTANCE is only
    // compared against the documented success threshold.
    #[allow(unsafe_code)]
    let code = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &file,
            w!("install"),
            None,
            windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD(0),
        )
    };
    // Per the docs a value above 32 means the launch succeeded; anything else is an error
    // code, and declining the UAC prompt surfaces as access-denied / cancelled.
    if (code.0 as isize) > 32 {
        Ok(())
    } else {
        Err(FxError::NotElevated(
            "administrator approval was declined or the installer could not start".into(),
        ))
    }
}

#[cfg(not(windows))]
pub fn install_helper() -> Result<(), FxError> {
    Err(FxError::UnsupportedFilesystem(
        "instant search uses the NTFS MFT and is only available on Windows".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_sits_beside_the_app_binary() {
        let p = helper_path().unwrap();
        assert_eq!(p.parent(), std::env::current_exe().unwrap().parent());
    }

    #[cfg(not(windows))]
    #[test]
    fn install_is_unsupported_off_windows() {
        assert!(matches!(
            install_helper(),
            Err(FxError::UnsupportedFilesystem(_))
        ));
    }
}
