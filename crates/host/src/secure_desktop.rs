//! App-facing control of the `streambeam-svc` secure-desktop service
//! (docs/SECURE_DESKTOP.md). This is the plumbing behind the single UI toggle
//! ("Allow access when locked / logged out"): the app polls [`status`] to
//! render the switch and calls [`set_enabled`] to flip it, which triggers
//! exactly one UAC prompt via `runas`. Nothing here needs the app itself to
//! be elevated — only the enable/disable action does, and Windows handles
//! that prompt.
//!
//! Packaging assumption: `streambeam-svc.exe` (and its worker
//! `streambeam-host.exe`) ship in the same directory as the app executable.
//! Absent that (e.g. a dev build where only the app is present), [`status`]
//! reports [`Status::Unsupported`] and the toggle hides itself.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not a Windows host, or the service binary isn't packaged alongside.
    Unsupported,
    /// Service binary present but not registered with the SCM.
    NotInstalled,
    /// Installed but not currently running.
    Stopped,
    /// Installed and running — streaming survives lock/logout.
    Running,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unsupported => "unsupported",
            Status::NotInstalled => "notinstalled",
            Status::Stopped => "stopped",
            Status::Running => "running",
        }
    }
}

#[cfg(windows)]
fn svc_exe() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("streambeam-svc.exe");
    candidate.exists().then_some(candidate)
}

/// Current service state, cheap enough to poll from the UI.
pub fn status() -> Status {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Some(svc) = svc_exe() else {
            return Status::Unsupported;
        };
        let out = std::process::Command::new(svc)
            .arg("status")
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match out {
            Ok(o) => match String::from_utf8_lossy(&o.stdout).trim() {
                "running" => Status::Running,
                "stopped" => Status::Stopped,
                _ => Status::NotInstalled,
            },
            Err(_) => Status::Unsupported,
        }
    }
    #[cfg(not(windows))]
    {
        Status::Unsupported
    }
}

/// Turn the service on (install + start) or off (stop + uninstall) via a
/// single elevated `runas` invocation of `streambeam-svc`. Blocks until the
/// elevated process finishes (or the user cancels the UAC prompt). The caller
/// re-polls [`status`] to reflect the result.
pub fn set_enabled(enable: bool) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, INFINITE, WaitForSingleObject,
        };
        use windows::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::core::{PCWSTR, w};

        let Some(svc) = svc_exe() else {
            return Err("secure-desktop service is not available in this build".into());
        };
        let verb = svc.into_os_string();
        let file: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(verb.as_os_str())
            .chain(std::iter::once(0))
            .collect();
        let params = if enable { w!("enable") } else { w!("disable") };

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"), // the UAC-elevation verb
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: params,
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info).map_err(|e| {
            // Canceling the UAC prompt surfaces here as ERROR_CANCELLED.
            format!("elevation was declined or failed: {e}")
        })?;
        if info.hProcess.is_invalid() {
            return Err("elevated helper did not start".into());
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let ok = GetExitCodeProcess(info.hProcess, &mut code).is_ok() && code == 0;
        let _ = CloseHandle(info.hProcess);
        if ok {
            Ok(())
        } else {
            Err(format!("streambeam-svc exited with code {code}"))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enable;
        Err("secure-desktop service is Windows-only".into())
    }
}
