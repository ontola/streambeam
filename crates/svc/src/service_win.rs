//! Windows service implementation (docs/SECURE_DESKTOP.md phase 1).
//!
//! SCM registration via the `windows-service` crate; the actual work is
//! supervising a **worker** process (`streambeam-host.exe`) launched into the
//! *active console session*. A plain service runs in Session 0, which has no
//! console/desktop at all — so it can't capture anything itself; it must place
//! the worker in the session where the display lives.
//!
//! **Which identity that worker runs under is not fixed**, because neither
//! choice can do the whole job (see [`WorkerToken`]): a SYSTEM token is the
//! only way onto the `Winlogon` (lock/login/UAC) desktop, and a user token is
//! the only way to use Windows.Graphics.Capture, without which a game holding
//! exclusive presentation cannot be captured at all. The supervisor watches
//! which desktop is at the front and runs the worker that can see it.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
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
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
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
    std::path::Path::new(&base)
        .join("streambeam")
        .join("worker.cfg")
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
    let worker_path_for_rule = worker.clone();
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
    // Do this here or the feature is self-defeating. Windows Firewall defaults
    // inbound to Block, and StreamBeam's allow-rules are per *program path* —
    // created when Windows prompts the first time the user runs a host by
    // hand. The worker lives at a different path and, as a SYSTEM service, can
    // never surface that prompt: it binds its port successfully and receives
    // nothing, forever, with no error anywhere. That cost two days of
    // debugging (2026-08-17/18), during which the only component able to
    // stream the lock screen was the only one unreachable. Install runs
    // elevated, which is exactly when the rule can be created, so create it.
    // While we still hold the elevation the install prompted for.
    if let Err(e) = allow_user_control() {
        eprintln!("could not grant the console user start/stop rights: {e}");
    }
    if let Err(e) = allow_worker_through_firewall(&worker_path_for_rule) {
        eprintln!(
            "warning: could not add the inbound firewall rule ({e}). The service \
             will run but clients may not reach it; add it by hand with:\n  \
             netsh advfirewall firewall add rule name=\"{FIREWALL_RULE}\" \
             dir=in action=allow protocol=UDP program=\"{}\" enable=yes profile=any",
            worker_path_for_rule.display()
        );
    }
    println!("installed {SERVICE_NAME}. Start it with: streambeam-svc start");
    Ok(())
}

/// Name used for both creation and removal, so `uninstall` can clean up
/// exactly what `install` added rather than leaving a stale hole open.
const FIREWALL_RULE: &str = "StreamBeam host (secure desktop worker)";

/// Allow inbound UDP to the worker binary.
///
/// Shells out to `netsh` rather than binding the firewall COM API: this runs
/// once, at install, already elevated, and the COM surface (`INetFwPolicy2`)
/// would be a lot of FFI for a single rule.
/// The ACE that lets whoever is signed in at the console start and stop this
/// service: `RP` (start), `WP` (stop) and `DT` (pause/continue) granted to
/// `IU`, the interactive-users group.
const USER_CONTROL_ACE: &str = "(A;;RPWPDT;;;IU)";

/// Let the logged-on user start and stop the service without elevation.
///
/// Not a convenience. Updating the *worker* is already elevation-free (see
/// `staging`), but updating the service binary itself means restarting it, and
/// by default only administrators may do that — so every change here cost a
/// UAC dance, which is precisely the friction that let the deployed build rot
/// a month behind the tree. Applied at install time (already elevated) and
/// available on its own for services installed before this existed.
///
/// The trade-off, stated plainly: anyone signed in at this machine's console
/// can now stop the host service, and stopping it is what removes remote
/// access. That is a fair trade on a personal machine — someone at your
/// keyboard has better options than this — and the wrong one on a shared or
/// managed box, where this should simply not be run.
pub fn allow_user_control() -> Res {
    let shown = std::process::Command::new("sc.exe")
        .args(["sdshow", SERVICE_NAME])
        .output()?;
    let sddl = String::from_utf8_lossy(&shown.stdout).trim().to_string();
    if !sddl.starts_with("D:") {
        return Err(format!("could not read the service's security descriptor: {sddl}").into());
    }
    if sddl.contains(USER_CONTROL_ACE) {
        println!("the console user can already start/stop {SERVICE_NAME}");
        return Ok(());
    }
    // The DACL is everything before the SACL; a new ACE has to land inside it,
    // not after `S:`, or it is silently parsed as an audit entry.
    let updated = match sddl.find("S:") {
        Some(at) => format!("{}{}{}", &sddl[..at], USER_CONTROL_ACE, &sddl[at..]),
        None => format!("{sddl}{USER_CONTROL_ACE}"),
    };
    let out = std::process::Command::new("sc.exe")
        .args(["sdset", SERVICE_NAME, &updated])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "sc sdset exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim()
        )
        .into());
    }
    println!("the console user may now start/stop {SERVICE_NAME} without elevation");
    Ok(())
}

fn allow_worker_through_firewall(worker: &std::path::Path) -> Res {
    // Remove first so re-installing (or moving the worker) doesn't accumulate
    // duplicate rules pointing at stale paths.
    let _ = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={FIREWALL_RULE}"),
        ])
        .output();
    let out = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &format!("name={FIREWALL_RULE}"),
            "dir=in",
            "action=allow",
            "protocol=UDP",
            &format!("program={}", worker.display()),
            "enable=yes",
            "profile=any",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "netsh exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim()
        )
        .into());
    }
    println!("firewall: allowed inbound UDP to {}", worker.display());
    Ok(())
}

fn remove_firewall_rule() {
    let _ = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={FIREWALL_RULE}"),
        ])
        .output();
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
    // Don't leave an inbound hole open for a service that no longer exists.
    remove_firewall_rule();
    println!("uninstalled {SERVICE_NAME}");
    Ok(())
}

pub fn start() -> Res {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::START | ServiceAccess::QUERY_STATUS,
    )?;
    // Starting an already-running service returns ERROR_SERVICE_ALREADY_RUNNING,
    // which the crate surfaces as "IO error in winapi call" — a message that
    // reads like a fault when the state is exactly what the user wanted.
    if let Ok(status) = service.query_status()
        && status.current_state == ServiceState::Running
    {
        println!("{SERVICE_NAME} is already running");
        return Ok(());
    }
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
    let mut probation: Option<Probation> = None;
    let mut last_refusal: Option<String> = None;
    let mut pending_token: Option<(WorkerToken, std::time::Instant)> = None;
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

        // Pick up a staged worker binary before deciding what to do with the
        // child, so an update lands on this tick rather than the next. The
        // firewall rule survives the swap untouched: it names the worker's
        // path, which is exactly what a rotation preserves.
        let mut swapped = false;
        match crate::staging::apply_staged(worker) {
            crate::staging::Applied::Swapped => {
                log("staged worker binary applied — restarting the worker to run it");
                swapped = true;
                probation = Some(Probation::new());
                last_refusal = None;
            }
            // A staged file that can't be used is usually a copy still in
            // flight, which fixes itself within a tick. Log the reason once
            // rather than every 500ms until someone notices.
            crate::staging::Applied::Refused(why) => {
                if last_refusal.as_deref() != Some(why.as_str()) {
                    log(&format!("staged worker ignored: {why}"));
                    last_refusal = Some(why);
                }
            }
            crate::staging::Applied::Nothing => last_refusal = None,
        }

        let exited = child.as_ref().is_some_and(|w| w.has_exited());
        let wrong_session = child.as_ref().is_some_and(|w| w.session != console);

        // Which identity the machine needs *right now*. Debounced, because a
        // UAC prompt flicks the input desktop to Winlogon and back, and every
        // switch costs the viewer their stream: worth it to reach a lock
        // screen, absurd for a prompt that was dismissed in half a second.
        let wanted = desired_token(console);
        let stable = match &pending_token {
            Some((t, since)) if *t == wanted => since.elapsed() >= TOKEN_SWITCH_DEBOUNCE,
            _ => {
                pending_token = Some((wanted, std::time::Instant::now()));
                false
            }
        };
        let wrong_token = stable && child.as_ref().is_some_and(|w| w.token != wanted);
        if wrong_token {
            log(&format!(
                "input desktop changed — restarting the worker as {}",
                wanted.label()
            ));
        }

        // A worker that dies immediately after an update is the update's fault
        // until proven otherwise — and this service is what provides remote
        // sign-in, so a bad binary would take away the very access needed to
        // replace it. Put the previous one back instead.
        if let Some(p) = probation.as_mut() {
            if exited {
                p.deaths += 1;
                if p.deaths >= PROBATION_DEATHS {
                    match crate::staging::roll_back(worker) {
                        Ok(()) => log(
                            "the staged worker exited immediately 3 times — rolled back to \
                             the previous binary (the rejected one is kept as \
                             streambeam-host.rejected.exe)",
                        ),
                        Err(e) => log(&format!(
                            "the staged worker keeps exiting and the rollback failed: {e}"
                        )),
                    }
                    probation = None;
                }
            } else if p.started.elapsed() >= PROBATION_PERIOD {
                log("the staged worker has stayed up — update looks good");
                probation = None;
            }
        }

        if swapped || exited || wrong_session || wrong_token {
            if let Some(w) = child.take() {
                w.terminate();
            }
        }
        if child.is_none() {
            child = spawn_worker(worker, console, wanted);
        }
    }
}

/// How long the machine must consistently want the other identity before the
/// worker is restarted into it. Long enough to ignore a UAC prompt that comes
/// and goes; short enough that reaching a lock screen still feels immediate.
const TOKEN_SWITCH_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How long a freshly-staged worker has to stay alive before the update counts
/// as good, and how many immediate deaths trigger a rollback.
const PROBATION_PERIOD: Duration = Duration::from_secs(20);
const PROBATION_DEATHS: u32 = 3;

/// Bookkeeping for a worker running a just-installed binary.
struct Probation {
    started: std::time::Instant,
    deaths: u32,
}

impl Probation {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            deaths: 0,
        }
    }
}

struct Worker {
    process: HANDLE,
    thread: HANDLE,
    session: u32,
    /// What it was started as, so the supervisor can notice when the machine
    /// now needs the other one.
    token: WorkerToken,
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

/// Which identity the worker runs under. Not a preference — each choice
/// forfeits something the other provides, so the supervisor switches:
///
/// * [`WorkerToken::ConsoleUser`] can use **Windows.Graphics.Capture**, the
///   only way to capture a game that has taken exclusive presentation. A
///   SYSTEM token cannot: measured 2026-08-18 on Mancave, `IsSupported()` and
///   `CreateForMonitor` both fail with `ERROR_SERVICE_DOES_NOT_EXIST`
///   (0x80070424) as `session 1, LOCAL SYSTEM`, because WinRT activation
///   needs per-user service instances that exist for a logged-on account and
///   not for SYSTEM. The identical binary captures fine as the user.
/// * [`WorkerToken::System`] can attach to the **Winlogon** desktop, which is
///   how the lock/login/UAC screen is streamed and signed into. No user-token
///   process may touch it, at all, by design.
///
/// Running one worker means giving up games or giving up remote sign-in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WorkerToken {
    System,
    ConsoleUser,
}

impl WorkerToken {
    fn label(self) -> &'static str {
        match self {
            WorkerToken::System => "LOCAL SYSTEM (secure desktop)",
            WorkerToken::ConsoleUser => "the console user (game capture)",
        }
    }
}

/// Is the secure desktop at the front? That is the one thing only a SYSTEM
/// worker can capture, so it decides which token the worker needs.
///
/// Anything that isn't clearly the user's own desktop is treated as secure:
/// failing to open the input desktop is itself what a user-token process sees
/// when Winlogon is up, and guessing "user" there would leave a locked machine
/// streaming nothing.
fn secure_desktop_is_active() -> bool {
    unsafe {
        let Ok(desk) = OpenInputDesktop(Default::default(), false, DESKTOP_READOBJECTS) else {
            return true;
        };
        let mut buf = [0u16; 64];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            // `HDESK` and `HANDLE` are the same thing to the API; the crate
            // just types desktops more precisely than user32 does.
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
        .is_ok();
        let _ = CloseDesktop(desk);
        if !ok {
            return true;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let name = String::from_utf16_lossy(&buf[..len]);
        // "Default" is the interactive desktop; "Winlogon" is the secure one,
        // and "Screen-saver" is neither but is equally out of a user's reach.
        !name.eq_ignore_ascii_case("Default")
    }
}

/// The logged-on console user's primary token, if anybody is signed in.
/// Requires SE_TCB_NAME, which SYSTEM has and nothing else does.
fn console_user_token(session: u32) -> Option<HANDLE> {
    unsafe {
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token).ok()?;
        Some(token)
    }
}

/// Which token the worker should be running under right now.
fn desired_token(session: u32) -> WorkerToken {
    if secure_desktop_is_active() {
        return WorkerToken::System;
    }
    match console_user_token(session) {
        Some(t) => {
            unsafe {
                let _ = CloseHandle(t);
            }
            WorkerToken::ConsoleUser
        }
        // Nobody signed in: there is no user to run as, and the login screen
        // is exactly what SYSTEM is for.
        None => WorkerToken::System,
    }
}

/// The console user's environment, plus the variables the worker needs.
///
/// A user-token process handed SYSTEM's environment gets SYSTEM's `%APPDATA%`,
/// `%TEMP%` and profile paths, which is a subtle way to produce a worker that
/// runs but behaves like a different account. The block is built for the real
/// user and our own variables appended.
unsafe fn user_environment(token: HANDLE, data_dir: Option<&str>) -> Option<Vec<u16>> {
    let mut block = std::ptr::null_mut();
    let mut env: Vec<u16> = Vec::new();
    unsafe {
        CreateEnvironmentBlock(&mut block, Some(token), false).ok()?;
        let p = block as *const u16;
        let mut i = 0usize;
        // The block is "KEY=VALUE\0KEY=VALUE\0\0" — copy up to and including
        // the final entry's terminator, leaving the block terminator to be
        // re-added after our own entries.
        loop {
            let c = *p.add(i);
            env.push(c);
            if c == 0 && *p.add(i + 1) == 0 {
                break;
            }
            i += 1;
        }
        let _ = DestroyEnvironmentBlock(block);
    }
    if let Some(dir) = data_dir {
        env.extend(wide(&format!("STREAMBEAM_DATA_DIR={dir}")));
    }
    // Deliberately *not* STREAMBEAM_SECURE_DESKTOP: desktop-following is a
    // SYSTEM-only trick, and a user worker attempting it would fail every tick.
    env.push(0);
    Some(env)
}

/// Launch the worker inside `session` (the active console session).
/// `None` on failure — the supervisor simply retries next tick.
fn spawn_worker(worker: &std::path::Path, session: u32, kind: WorkerToken) -> Option<Worker> {
    unsafe {
        // The token the worker runs under, plus (for a user worker) the
        // environment that has to travel with it.
        let mut env_block: Option<Vec<u16>> = None;
        let token = match kind {
            WorkerToken::ConsoleUser => {
                let Some(user) = console_user_token(session) else {
                    log("spawn: nobody signed in at the console — using SYSTEM instead");
                    return spawn_worker(worker, session, WorkerToken::System);
                };
                match user_environment(user, recorded_worker_data_dir().as_deref()) {
                    Some(env) => env_block = Some(env),
                    None => {
                        // Rather than silently run the user's worker with
                        // SYSTEM's profile — a worker that starts and then
                        // behaves like the wrong account.
                        log("spawn: CreateEnvironmentBlock failed for the console user");
                        let _ = CloseHandle(user);
                        return None;
                    }
                }
                user
            }
            WorkerToken::System => {
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

                // 2. Duplicate it into a primary token we can retarget + hand
                //    to CreateProcessAsUser.
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

                // 3. Point the duplicated SYSTEM token at the console session,
                //    so the worker lands where the display is — the crux of the
                //    whole thing. A user token already names its own session.
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

                // 4. Share the app's host identity + pairings: point the worker
                //    at the user's data dir (recorded at enable time). Set it on
                //    *this* process so the child inherits it via the NULL
                //    environment below — SYSTEM can read/write the user's
                //    %APPDATA% fine. Without this the worker would mint its own
                //    identity under SYSTEM's profile and the client would have
                //    to re-pair. The user worker gets the same directory
                //    through its own environment block, so both identities
                //    match and switching between them is invisible to a client.
                if let Some(dir) = recorded_worker_data_dir() {
                    // Already inside spawn_worker's unsafe block.
                    std::env::set_var("STREAMBEAM_DATA_DIR", &dir);
                }
                // Opt the worker into desktop-following (capture + input track
                // the lock/login/UAC desktop). The in-app host never sets this,
                // so its injection stays on the single interactive desktop —
                // see `desktop_win::following_enabled`.
                std::env::set_var("STREAMBEAM_SECURE_DESKTOP", "1");
                dup
            }
        };

        // `winsta0\default` is the interactive desktop; the SYSTEM worker
        // switches its own threads onto Winlogon as needed.
        let mut cmd = wide(&format!("\"{}\"", worker.display()));
        let mut desktop = wide("winsta0\\default");
        let mut si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: windows::core::PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        // A user worker must not put a console window on the user's own
        // desktop; the SYSTEM one keeps the console it has always had.
        let window_flag = match kind {
            WorkerToken::ConsoleUser => CREATE_NO_WINDOW,
            WorkerToken::System => CREATE_NEW_CONSOLE,
        };
        let created = CreateProcessAsUserW(
            Some(token),
            None,
            Some(windows::core::PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | window_flag,
            // None = inherit SYSTEM's environment (worker uses its own data
            // dir); Some = the console user's own environment.
            env_block
                .as_ref()
                .map(|e| e.as_ptr() as *const std::ffi::c_void),
            None,
            &mut si,
            &mut pi,
        );
        let _ = CloseHandle(token);
        if created.is_err() {
            log(&format!("spawn: CreateProcessAsUser failed: {:?}", created));
            return None;
        }
        log(&format!(
            "worker launched in session {session} as {} (pid {})",
            kind.label(),
            pi.dwProcessId
        ));
        Some(Worker {
            process: pi.hProcess,
            thread: pi.hThread,
            session,
            token: kind,
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
    let path = std::path::Path::new(&dir)
        .join("streambeam")
        .join("svc.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{msg}");
    }
}
