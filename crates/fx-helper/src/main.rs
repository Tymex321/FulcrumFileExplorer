// `unwrap` is denied in production code; in tests it is the clearest way to assert success.
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! # fx-helper — the elevated index service
//!
//! Windows only; on other platforms this binary exists but does nothing, so the workspace
//! still builds on macOS.
//!
//! ## Why this process exists
//!
//! Reading the NTFS Master File Table means opening a raw volume handle (`\\.\C:`), which
//! requires administrator rights. Elevating the whole file explorer would mean a UAC
//! prompt on every launch and would run a GUI app with WebView2 as administrator — bad UX
//! and a bad security posture.
//!
//! So privilege is isolated here instead. This is a small Windows service that holds the
//! raw handle and the index, and answers queries over
//! [`fx_core::HELPER_PIPE_NAME`]. The main app runs unelevated (`asInvoker`) and connects
//! to it. The user sees exactly one UAC prompt, ever: when they click "Enable instant
//! search".
//!
//! ## Modes
//!
//! ```text
//! fx-helper install     register and start the service (requires elevation; run via runas)
//! fx-helper uninstall   stop and deregister it
//! fx-helper run         service entry point, called by the SCM
//! fx-helper debug       run in the foreground, for development
//! ```
//!
//! ## Security
//!
//! The protocol, its framing limits and its handshake live in [`fx_index::ipc`], shared
//! with the client. What this binary adds:
//!
//! - **An explicit pipe DACL**: SYSTEM, Administrators and the user who installed the
//!   service, nobody else. The installer records that user's SID as a service argument.
//! - **`PIPE_REJECT_REMOTE_CLIENTS`**: the pipe is never reachable over SMB.
//! - **`FILE_FLAG_FIRST_PIPE_INSTANCE`** on the first instance, so the service refuses to
//!   start rather than share a name another process squatted first.
//! - **A bounded number of concurrent clients**, so connection floods cannot exhaust
//!   threads.
//! - **A private state directory**: checkpoints list every file name on the volume,
//!   including other users' private ones, and `%ProgramData%` subfolders are readable by
//!   all users by default. The directory gets a protected DACL of SYSTEM +
//!   Administrators.
//! - **Read-only, forever.** The protocol has no write or execute operation and must never
//!   gain one: that would turn a query channel into privilege escalation.

/// Service identity, shared by the installer and the service entry point so they cannot
/// disagree about it.
pub const SERVICE_NAME: &str = "FulcrumIndex";
pub const SERVICE_DISPLAY_NAME: &str = "Fulcrum Index Service";
pub const SERVICE_DESCRIPTION: &str =
    "Keeps Fulcrum's instant-search index current by reading the NTFS change journal. Read-only.";

/// Most clients served at once. The app needs a handful (a pooled connection or two per
/// window plus one change feed); anything beyond this is refused.
pub const MAX_CLIENTS: usize = 16;

/// Hex-encodes a binary SID for passing as a service launch argument.
pub fn sid_to_hex(sid: &[u8]) -> String {
    sid.iter().map(|b| format!("{b:02x}")).collect()
}

/// Inverse of [`sid_to_hex`]. Rejects anything that is not an even-length hex string of a
/// plausible SID size (8..=68 bytes), so a tampered service argument cannot smuggle an
/// oversized buffer into the ACL builder.
pub fn sid_from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !(16..=136).contains(&s.len()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Parsed command line: the mode and any `--allow-sid <hex>` grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub mode: String,
    pub allowed_sids: Vec<Vec<u8>>,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut it = args.into_iter();
    let mode = it.next().unwrap_or_else(|| "debug".into());
    let mut allowed_sids = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--allow-sid" => {
                let v = it.next().ok_or("--allow-sid needs a value")?;
                allowed_sids.push(sid_from_hex(&v).ok_or_else(|| format!("invalid SID {v:?}"))?);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Args { mode, allowed_sids })
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fx_helper=info,fx_index=info".into()),
        )
        .init();

    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("usage: fx-helper [install|uninstall|run|debug] [--allow-sid <hex>]");
            std::process::exit(2);
        }
    };

    #[cfg(windows)]
    {
        let result = match args.mode.as_str() {
            "install" => service::install(),
            "uninstall" => service::uninstall(),
            "run" => service::run_as_service(args.allowed_sids),
            "debug" => service::run_foreground(args.allowed_sids),
            other => Err(format!("unknown mode: {other}")),
        };
        if let Err(e) = result {
            tracing::error!("{e}");
            eprintln!("fx-helper: {e}");
            std::process::exit(1);
        }
    }

    #[cfg(not(windows))]
    {
        // The helper has no purpose off Windows: there is no MFT and no service manager.
        // It still builds so that `cargo build --workspace` is green on the development
        // machine.
        tracing::info!(
            mode = %args.mode,
            "fx-helper is a Windows-only component; nothing to do on this platform"
        );
    }
}

#[cfg(windows)]
mod service {
    //! Service lifecycle and the named-pipe server.

    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, OnceLock};
    use std::time::Duration;

    use fx_core::IndexBackend;
    use fx_index::ipc::{serve_connection, WatchHub};
    use fx_index::MftIndex;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{sid_to_hex, MAX_CLIENTS, SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME};

    type Res<T> = std::result::Result<T, String>;

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    /// `%ProgramData%\Fulcrum`, created with a private DACL.
    fn state_dir() -> Res<std::path::PathBuf> {
        let base = std::env::var_os("ProgramData").ok_or("ProgramData is not set")?;
        let dir = std::path::Path::new(&base).join("Fulcrum");
        std::fs::create_dir_all(&dir).map_err(err)?;
        win32::protect_directory(&dir).map_err(err)?;
        Ok(dir)
    }

    pub fn install() -> Res<()> {
        let user = win32::current_user_sid().map_err(err)?;
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .map_err(|e| format!("cannot open the service manager (run elevated): {e}"))?;

        // Reinstalling replaces the old registration, e.g. after an upgrade moved the exe.
        if let Ok(old) = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        ) {
            let _ = old.stop();
            wait_stopped(&old);
            old.delete().map_err(err)?;
            drop(old);
            // Deletion completes once the last handle closes; give the SCM a moment.
            std::thread::sleep(Duration::from_millis(500));
        }

        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe().map_err(err)?,
            launch_arguments: vec![
                OsString::from("run"),
                OsString::from("--allow-sid"),
                OsString::from(sid_to_hex(&user)),
            ],
            dependencies: vec![],
            account_name: None, // LocalSystem: the account that can open \\.\C:
            account_password: None,
        };
        let svc = manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .map_err(err)?;
        svc.set_description(SERVICE_DESCRIPTION).map_err(err)?;
        svc.start::<&str>(&[]).map_err(err)?;
        tracing::info!("installed and started {SERVICE_NAME}");
        Ok(())
    }

    pub fn uninstall() -> Res<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(err)?;
        let svc = manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
            )
            .map_err(|e| format!("{SERVICE_NAME} is not installed: {e}"))?;
        let _ = svc.stop();
        wait_stopped(&svc);
        svc.delete().map_err(err)?;
        // The checkpoint lists every file name on the volume; do not leave it behind.
        if let Some(base) = std::env::var_os("ProgramData") {
            let _ = std::fs::remove_dir_all(std::path::Path::new(&base).join("Fulcrum"));
        }
        tracing::info!("uninstalled {SERVICE_NAME}");
        Ok(())
    }

    fn wait_stopped(svc: &windows_service::service::Service) {
        for _ in 0..50 {
            match svc.query_status() {
                Ok(s) if s.current_state != ServiceState::Stopped => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                _ => return,
            }
        }
    }

    static ALLOWED: OnceLock<Vec<Vec<u8>>> = OnceLock::new();

    pub fn run_as_service(allowed: Vec<Vec<u8>>) -> Res<()> {
        let _ = ALLOWED.set(allowed);
        service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(err)
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!("service failed: {e}");
        }
    }

    fn status(state: ServiceState, accept: ServiceControlAccept) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }
    }

    fn run_service() -> Res<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let stop_h = Arc::clone(&stop);
        let handler = move |ev| match ev {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_h.store(true, Ordering::SeqCst);
                let _ = stop_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            // Interrogate, plus anything else: the SCM only delivers the controls declared
            // in `controls_accepted` (STOP, SHUTDOWN) and Interrogate, so there is nothing
            // else to refuse.
            _ => ServiceControlHandlerResult::NoError,
        };
        let handle = service_control_handler::register(SERVICE_NAME, handler).map_err(err)?;
        // Report running *now*: the SCM kills a service that is slow to report, and the
        // first scan of a large volume takes seconds. Indexing happens on a worker.
        handle
            .set_service_status(status(
                ServiceState::Running,
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            ))
            .map_err(err)?;

        let allowed = ALLOWED.get().cloned().unwrap_or_default();
        let result = serve(allowed, Arc::clone(&stop), Some(stop_rx));

        handle
            .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))
            .map_err(err)?;
        result
    }

    pub fn run_foreground(mut allowed: Vec<Vec<u8>>) -> Res<()> {
        if allowed.is_empty() {
            allowed.push(win32::current_user_sid().map_err(err)?);
        }
        tracing::info!(
            "serving {} in the foreground; Ctrl+C to stop",
            fx_core::HELPER_PIPE_NAME
        );
        serve(allowed, Arc::new(AtomicBool::new(false)), None)
    }

    /// Builds the index on a worker thread and serves the pipe until stopped.
    fn serve(
        allowed: Vec<Vec<u8>>,
        stop: Arc<AtomicBool>,
        stop_rx: Option<mpsc::Receiver<()>>,
    ) -> Res<()> {
        let index = MftIndex::all_volumes().with_state_dir(state_dir()?);
        let elevated = index.is_available();
        let hub = Arc::new(WatchHub::new(Arc::new(index)));

        {
            let hub = Arc::clone(&hub);
            std::thread::Builder::new()
                .name("fx-index-build".into())
                .spawn(move || {
                    let idx = hub.inner();
                    if !idx.restore() {
                        if let Err(e) = idx.scan(&fx_core::NullSink) {
                            tracing::error!("initial scan failed: {e}");
                        }
                    }
                    if let Err(e) = hub.start() {
                        tracing::error!("cannot follow the change journal: {e}");
                    }
                })
                .map_err(err)?;
        }

        // On stop, wake the accept loop (blocked in ConnectNamedPipe) by connecting to
        // ourselves; it then sees the flag and returns.
        if let Some(rx) = stop_rx {
            std::thread::spawn(move || {
                if rx.recv().is_ok() {
                    let _ = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(fx_core::HELPER_PIPE_NAME);
                }
            });
        }

        let sd = win32::PipeSecurity::new(&allowed).map_err(err)?;
        let active = Arc::new(AtomicUsize::new(0));
        let mut first = true;
        while !stop.load(Ordering::SeqCst) {
            let pipe = win32::create_pipe_instance(&sd, first).map_err(err)?;
            first = false;
            if let Err(e) = win32::wait_for_client(&pipe) {
                tracing::warn!("pipe connect failed: {e}");
                continue;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if active.load(Ordering::SeqCst) >= MAX_CLIENTS {
                tracing::warn!("refusing client: {MAX_CLIENTS} already connected");
                continue; // Dropping `pipe` closes it.
            }
            let reader = match pipe.try_clone() {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("cannot clone pipe handle: {e}");
                    continue;
                }
            };
            active.fetch_add(1, Ordering::SeqCst);
            let hub = Arc::clone(&hub);
            let active = Arc::clone(&active);
            let spawned = std::thread::Builder::new()
                .name("fx-helper-client".into())
                .spawn(move || {
                    let backend: &dyn IndexBackend = &*hub;
                    if let Err(e) = serve_connection(backend, reader, pipe, elevated) {
                        tracing::debug!("client connection ended: {e}");
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            if spawned.is_err() {
                tracing::warn!("cannot spawn client thread");
            }
        }
        hub.stop();
        hub.inner().clear().map_err(err)?;
        Ok(())
    }

    #[allow(unsafe_code)]
    mod win32 {
        //! The raw Win32 calls: token SIDs, DACL construction, named pipe instances.
        //! Every `unsafe` block in the helper is here.

        use std::ffi::c_void;
        use std::fs::File;
        use std::io;
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use std::path::Path;

        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{
            CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_ALL, HANDLE, INVALID_HANDLE_VALUE,
        };
        use windows::Win32::Security::{
            AddAccessAllowedAceEx, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
            InitializeAcl, InitializeSecurityDescriptor, IsValidSid, SetFileSecurityW,
            SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TokenUser,
            WinBuiltinAdministratorsSid, WinLocalSystemSid, ACE_FLAGS, ACL, ACL_REVISION,
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            SECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER, WELL_KNOWN_SID_TYPE,
        };
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, PIPE_ACCESS_DUPLEX,
        };
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        fn os_err(e: windows::core::Error) -> io::Error {
            io::Error::other(e.to_string())
        }

        /// The SID of the user running this process, as raw bytes.
        pub fn current_user_sid() -> io::Result<Vec<u8>> {
            let mut token = HANDLE::default();
            // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing; `token`
            // is a valid out-pointer and is closed below.
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
                .map_err(os_err)?;
            let mut len = 0u32;
            // SAFETY: a size query with no buffer; failure with a length is expected.
            let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
            // u64 storage keeps the TOKEN_USER header suitably aligned.
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            // SAFETY: `buf` holds at least `len` bytes, as just reported.
            let res = unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(buf.as_mut_ptr().cast()),
                    len,
                    &mut len,
                )
            };
            // SAFETY: `token` was opened above and is not used after this.
            let _ = unsafe { CloseHandle(token) };
            res.map_err(os_err)?;
            // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER whose
            // SID points inside the same buffer, which is still alive.
            let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
            sid_bytes(sid)
        }

        fn sid_bytes(sid: PSID) -> io::Result<Vec<u8>> {
            // SAFETY: callers pass a SID pointer into a live buffer.
            unsafe {
                if !IsValidSid(sid).as_bool() {
                    return Err(io::Error::other("invalid SID"));
                }
                let n = GetLengthSid(sid) as usize;
                Ok(std::slice::from_raw_parts(sid.0 as *const u8, n).to_vec())
            }
        }

        fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
            let mut buf = vec![0u8; 68]; // SECURITY_MAX_SID_SIZE
            let mut len = buf.len() as u32;
            // SAFETY: `buf` is `len` bytes long.
            unsafe {
                CreateWellKnownSid(kind, None, Some(PSID(buf.as_mut_ptr().cast())), &mut len)
            }
            .map_err(os_err)?;
            buf.truncate(len as usize);
            Ok(buf)
        }

        /// A self-contained absolute security descriptor and the buffers it points into.
        /// The fields are kept alive for as long as the descriptor is in use.
        pub struct PipeSecurity {
            sd: Box<SECURITY_DESCRIPTOR>,
            _acl: Vec<u64>,
            _sids: Vec<Vec<u8>>,
        }

        impl PipeSecurity {
            /// Pipe DACL: SYSTEM, Administrators and `extra` (the installing user) may
            /// read and write; everyone else is denied by omission.
            pub fn new(extra: &[Vec<u8>]) -> io::Result<Self> {
                let mut sids = vec![
                    well_known(WinLocalSystemSid)?,
                    well_known(WinBuiltinAdministratorsSid)?,
                ];
                sids.extend(extra.iter().cloned());
                let mask = (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0;
                Self::build(sids, mask, ACE_FLAGS(0), false)
            }

            /// Directory DACL: SYSTEM and Administrators only, inherited by everything
            /// created inside, and protected from inheriting `%ProgramData%`'s grants.
            fn private_dir() -> io::Result<Self> {
                let sids = vec![
                    well_known(WinLocalSystemSid)?,
                    well_known(WinBuiltinAdministratorsSid)?,
                ];
                Self::build(
                    sids,
                    GENERIC_ALL.0,
                    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    true,
                )
            }

            fn build(
                sids: Vec<Vec<u8>>,
                mask: u32,
                inherit: ACE_FLAGS,
                protect: bool,
            ) -> io::Result<Self> {
                for s in &sids {
                    sid_bytes(PSID(s.as_ptr() as *mut c_void))?;
                }
                // ACL header (8) + per ACE: header (8) + mask is counted in it + SID.
                let size = 8 + sids.iter().map(|s| 8 + s.len()).sum::<usize>();
                let mut acl = vec![0u64; size.div_ceil(8)];
                let pacl = acl.as_mut_ptr() as *mut ACL;
                let mut sd = Box::new(SECURITY_DESCRIPTOR::default());
                let psd = PSECURITY_DESCRIPTOR(&mut *sd as *mut _ as *mut c_void);
                // SAFETY: `acl` is `size` bytes and aligned; every SID was validated above
                // and outlives the calls; `sd` is a boxed, writable descriptor.
                unsafe {
                    InitializeAcl(pacl, (acl.len() * 8) as u32, ACL_REVISION).map_err(os_err)?;
                    for s in &sids {
                        AddAccessAllowedAceEx(
                            pacl,
                            ACL_REVISION,
                            inherit,
                            mask,
                            PSID(s.as_ptr() as *mut c_void),
                        )
                        .map_err(os_err)?;
                    }
                    InitializeSecurityDescriptor(psd, 1).map_err(os_err)?;
                    SetSecurityDescriptorDacl(psd, true, Some(pacl), false).map_err(os_err)?;
                    if protect {
                        SetSecurityDescriptorControl(psd, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                            .map_err(os_err)?;
                    }
                }
                Ok(Self {
                    sd,
                    _acl: acl,
                    _sids: sids,
                })
            }

            fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
                PSECURITY_DESCRIPTOR(&*self.sd as *const _ as *mut c_void)
            }
        }

        /// Replaces a directory's DACL with the private one.
        pub fn protect_directory(dir: &Path) -> io::Result<()> {
            let sec = PipeSecurity::private_dir()?;
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated; `sec` outlives the call.
            let ok = unsafe {
                SetFileSecurityW(
                    PCWSTR::from_raw(wide.as_ptr()),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    sec.as_ptr(),
                )
            };
            if ok.as_bool() {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        /// Creates one pipe instance, returned as a `File` that owns the handle.
        pub fn create_pipe_instance(sec: &PipeSecurity, first: bool) -> io::Result<File> {
            let name: Vec<u16> = fx_core::HELPER_PIPE_NAME
                .encode_utf16()
                .chain([0])
                .collect();
            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sec.as_ptr().0,
                bInheritHandle: false.into(),
            };
            let mut open = PIPE_ACCESS_DUPLEX;
            if first {
                open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }
            const BUF: u32 = 64 * 1024;
            // SAFETY: `name` is NUL-terminated; `attrs` and the descriptor it points to
            // outlive the call.
            let h = unsafe {
                CreateNamedPipeW(
                    PCWSTR::from_raw(name.as_ptr()),
                    FILE_FLAGS_AND_ATTRIBUTES(open.0),
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    BUF,
                    BUF,
                    0,
                    Some(&attrs),
                )
            };
            if h == INVALID_HANDLE_VALUE || h.is_invalid() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `h` is a fresh, valid handle we own; `File` takes ownership.
            Ok(unsafe { File::from_raw_handle(h.0) })
        }

        /// Blocks until a client connects to this instance.
        pub fn wait_for_client(pipe: &File) -> io::Result<()> {
            use std::os::windows::io::AsRawHandle;
            let h = HANDLE(pipe.as_raw_handle());
            // SAFETY: `h` is the live pipe handle owned by `pipe`.
            match unsafe { ConnectNamedPipe(h, None) } {
                Ok(()) => Ok(()),
                // The client connected between Create and Connect: still a connection.
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
                Err(e) => Err(os_err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sids_round_trip_through_hex() {
        // S-1-5-21-... is 28 bytes; any byte value must survive.
        let sid: Vec<u8> = (0u8..28).map(|i| i.wrapping_mul(37)).collect();
        assert_eq!(sid_from_hex(&sid_to_hex(&sid)), Some(sid));
    }

    #[test]
    fn malformed_sid_arguments_are_rejected() {
        assert_eq!(sid_from_hex("abc"), None, "odd length");
        assert_eq!(sid_from_hex("zz".repeat(10).as_str()), None, "not hex");
        assert_eq!(sid_from_hex("00"), None, "too short for a SID");
        assert_eq!(sid_from_hex(&"00".repeat(200)), None, "too long for a SID");
    }

    #[test]
    fn args_parse_mode_and_grants() {
        let a = parse_args(["run", "--allow-sid", &"01".repeat(12)].map(String::from)).unwrap();
        assert_eq!(a.mode, "run");
        assert_eq!(a.allowed_sids, vec![vec![1u8; 12]]);
        assert_eq!(parse_args(Vec::<String>::new()).unwrap().mode, "debug");
        assert!(parse_args(["run", "--allow-sid"].map(String::from)).is_err());
        assert!(parse_args(["run", "--bogus"].map(String::from)).is_err());
    }
}
