//! Windows service implementation (docs/SECURE_DESKTOP.md phase 1).
//!
//! SCM registration via the `windows-service` crate; the actual work is
//! supervising a **worker** process (`streambeam-host.exe`) launched into the
//! *active console session* as SYSTEM. A plain service runs in Session 0,
//! which has no console/desktop at all — so it can't capture anything itself;
//! it must place the worker in the session where the display lives. Doing
//! that with a SYSTEM token (rather than a logged-on user's) is deliberate:
//! it's the same mechanism phase 2 needs to attach to the `Winlogon`
//! (lock/login/UAC) desktop, which no user-token process may touch.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "StreamBeamHost";
const DISPLAY_NAME: &str = "StreamBeam Host Service";

type Res = Result<(), Box<dyn std::error::Error>>;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Default worker path: `streambeam-host.exe` beside this binary.
fn default_worker() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("streambeam-host.exe")))
        .unwrap_or_else(|| std::path::PathBuf::from("streambeam-host.exe"))
}

/// Where the console user's data dir is recorded, so the SYSTEM worker can
/// reuse the *app's* host identity + pairings instead of minting its own
/// under SYSTEM's profile (which would force a re-pair). `enable` (running
/// elevated *as the user*) writes it; the service (SYSTEM) reads it.
fn worker_cfg_path() -> std::path::PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
    std::path::Path::new(&base).join("streambeam").join("worker.cfg")
}

/// Record `%APPDATA%\streambeam` (the current user's host data dir) for the
/// worker. Called from `enable`, which runs as the elevated user, so `APPDATA`
/// is the real user's — not SYSTEM's.
pub fn record_worker_data_dir_pub() {
    record_worker_data_dir();
}

fn record_worker_data_dir() {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = std::path::Path::new(&appdata).join("streambeam");
        let cfg = worker_cfg_path();
        if let Some(parent) = cfg.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cfg, dir.to_string_lossy().as_bytes());
    }
}

/// The recorded user data dir, if any — the worker's `STREAMBEAM_DATA_DIR`.
fn recorded_worker_data_dir() -> Option<String> {
    std::fs::read_to_string(worker_cfg_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// --- SCM control (install / uninstall / start / stop) --------------------

pub fn install(worker: Option<String>) -> Res {
    let worker = worker
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_worker);
    if !worker.exists() {
        return Err(format!(
            "worker not found at {} — build streambeam-host and pass --worker <path> if it's elsewhere",
            worker.display()
        )
        .into());
    }
    // The service binary itself is what the SCM launches (with `run`); it then
    // spawns the worker. Store the worker path as a launch argument so the
    // service can find it at SYSTEM-start time (its cwd/relative paths differ).
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: vec![OsString::from("run"), worker.into_os_string()],
        dependencies: vec![],
        account_name: None, // None = LocalSystem
        account_password: None,
    };
    let service = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG)?;
    service.set_description(
        "Keeps StreamBeam streaming across logout and (later) the lock/login screen.",
    )?;
    println!("installed {SERVICE_NAME}. Start it with: streambeam-svc start");
    Ok(())
}

pub fn uninstall() -> Res {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    )?;
    // Best-effort stop before delete so it doesn't linger until reboot.
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    service.delete()?;
    println!("uninstalled {SERVICE_NAME}");
    Ok(())
}

pub fn start() -> Res {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::START)?;
    service.start(&[] as &[&OsStr])?;
    println!("started {SERVICE_NAME}");
    Ok(())
}

pub fn stop() -> Res {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::STOP)?;
    service.stop()?;
    println!("stopping {SERVICE_NAME}");
    Ok(())
}

/// stop + uninstall in one call (the app's toggle-off, run elevated).
pub fn disable() -> Res {
    // `uninstall` already best-effort stops before deleting.
    uninstall()
}

/// One-word service state for the app's toggle, queried without elevation:
/// `unsupported` (no service exe context) is not reachable here — this binary
/// *is* the service — so we report `notinstalled` / `stopped` / `running`.
/// SCM read access is available to normal users, so no UAC needed.
pub fn status_word() -> &'static str {
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    else {
        return "notinstalled";
    };
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => match service.query_status() {
            Ok(status) if status.current_state == ServiceState::Running => "running",
            Ok(_) => "stopped",
            Err(_) => "stopped",
        },
        // Open failing is overwhelmingly "service doesn't exist".
        Err(_) => "notinstalled",
    }
}

// --- Service runtime (SCM entry) -----------------------------------------

windows_service::define_windows_service!(ffi_service_main, service_main);

pub fn run_from_scm() -> Res {
    // Hands control to the SCM, which calls `ffi_service_main` on the service
    // thread. Blocks until the service stops.
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(arguments: Vec<OsString>) {
    if let Err(e) = run_service(arguments) {
        log(&format!("service exited with error: {e}"));
    }
}

fn run_service(arguments: Vec<OsString>) -> Res {
    // arguments[0] is the service name; [1] is our `run`, [2] the worker path
    // (see `install`). Fall back to the default beside us.
    let worker = arguments
        .get(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_worker);

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

    let running = |accept: ServiceControlAccept, state: ServiceState| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status_handle.set_service_status(running(
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ServiceState::Running,
    ))?;
    log("service running");

    supervise_worker(&worker, &shutdown_rx);

    status_handle.set_service_status(running(
        ServiceControlAccept::empty(),
        ServiceState::Stopped,
    ))?;
    log("service stopped");
    Ok(())
}

/// Keep exactly one worker alive in the current console session. Relaunch on
/// exit or when the console session changes (logon/logoff/fast-user-switch),
/// so capture always targets the session actually at the screen. Returns when
/// a stop is requested.
fn supervise_worker(worker: &std::path::Path, shutdown_rx: &std::sync::mpsc::Receiver<()>) {
    let mut child: Option<Worker> = None;
    loop {
        // Stop requested?
        if shutdown_rx.recv_timeout(Duration::from_millis(500)).is_ok() {
            if let Some(w) = child.take() {
                w.terminate();
            }
            return;
        }
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        // 0xFFFFFFFF = no one at the console (rare, transient) — wait it out.
        if console == u32::MAX {
            continue;
        }
        match &child {
            // Worker gone, or now in the wrong session → (re)spawn.
            Some(w) if w.session != console || w.has_exited() => {
                if let Some(w) = child.take() {
                    w.terminate();
                }
                child = spawn_worker(worker, console);
            }
            Some(_) => {}
            None => child = spawn_worker(worker, console),
        }
    }
}

struct Worker {
    process: HANDLE,
    thread: HANDLE,
    session: u32,
}

impl Worker {
    fn has_exited(&self) -> bool {
        unsafe {
            let mut code = 0u32;
            // STILL_ACTIVE (259) means running; anything else = exited.
            GetExitCodeProcess(self.process, &mut code).is_ok() && code != 259
        }
    }

    fn terminate(self) {
        unsafe {
            let _ = TerminateProcess(self.process, 1);
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

/// Launch the worker as SYSTEM inside `session` (the active console session).
/// `None` on failure — the supervisor simply retries next tick.
fn spawn_worker(worker: &std::path::Path, session: u32) -> Option<Worker> {
    unsafe {
        // 1. Our own (SYSTEM) token.
        let mut self_token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut self_token,
        )
        .is_err()
        {
            log("spawn: OpenProcessToken failed");
            return None;
        }

        // 2. Duplicate it into a primary token we can retarget + hand to
        //    CreateProcessAsUser.
        let mut dup = HANDLE::default();
        let dup_ok = DuplicateTokenEx(
            self_token,
            TOKEN_ALL_ACCESS,
            None,
            SecurityIdentification,
            TokenPrimary,
            &mut dup,
        )
        .is_ok();
        let _ = CloseHandle(self_token);
        if !dup_ok {
            log("spawn: DuplicateTokenEx failed");
            return None;
        }

        // 3. Point the duplicated SYSTEM token at the console session, so the
        //    worker lands where the display is — the crux of the whole thing.
        let sid = session;
        if SetTokenInformation(
            dup,
            TokenSessionId,
            &sid as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_err()
        {
            log("spawn: SetTokenInformation(session) failed");
            let _ = CloseHandle(dup);
            return None;
        }

        // 4. Share the app's host identity + pairings: point the worker at
        //    the user's data dir (recorded at enable time). Set it on *this*
        //    process so the child inherits it via the NULL environment below
        //    — SYSTEM can read/write the user's %APPDATA% fine. Without this
        //    the worker would mint its own identity under SYSTEM's profile
        //    and the client would have to re-pair.
        if let Some(dir) = recorded_worker_data_dir() {
            // Already inside spawn_worker's unsafe block.
            std::env::set_var("STREAMBEAM_DATA_DIR", &dir);
        }
        // Opt the worker into desktop-following (capture + input track the
        // lock/login/UAC desktop). The in-app host never sets this, so its
        // injection stays on the single interactive desktop — see
        // `desktop_win::following_enabled`.
        std::env::set_var("STREAMBEAM_SECURE_DESKTOP", "1");

        // `winsta0\default` is the interactive desktop; phase 2 switches the
        // worker's own threads onto Winlogon as needed.
        let mut cmd = wide(&format!("\"{}\"", worker.display()));
        let mut desktop = wide("winsta0\\default");
        let mut si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: windows::core::PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            Some(dup),
            None,
            Some(windows::core::PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_CONSOLE,
            None, // inherit SYSTEM environment (worker uses its own data dir)
            None,
            &mut si,
            &mut pi,
        );
        let _ = CloseHandle(dup);
        if created.is_err() {
            log(&format!("spawn: CreateProcessAsUser failed: {:?}", created));
            return None;
        }
        log(&format!("worker launched in session {session} (pid {})", pi.dwProcessId));
        Some(Worker {
            process: pi.hProcess,
            thread: pi.hThread,
            session,
        })
    }
}

/// Wait up to `ms` for the worker; used only if a caller wants to block on it
/// (kept for the supervisor's future exit-driven relaunch — polling is fine
/// for phase 1's 500ms cadence).
#[allow(dead_code)]
fn wait_worker(worker: &Worker, ms: u32) -> bool {
    unsafe { WaitForSingleObject(worker.process, ms) == windows::Win32::Foundation::WAIT_OBJECT_0 }
}

/// Service logging: SYSTEM services have no console, so append to a file next
/// to the worker's own log. Best-effort — a failed log line must never take
/// the service down.
fn log(msg: &str) {
    use std::io::Write;
    let dir = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
    let path = std::path::Path::new(&dir).join("streambeam").join("svc.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{msg}");
    }
}
