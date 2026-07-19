// Win32 desktop-switching for the secure-desktop worker (docs/SECURE_DESKTOP.md
// phase 2). Only meaningful when the process runs as LOCAL SYSTEM under
// `streambeam-svc`; in a normal user session every call resolves to the
// desktop we're already on, so it's a harmless no-op there.
#![allow(unsafe_op_in_unsafe_fn)]

//! Attach the *calling thread* to whatever desktop currently owns user input
//! — `Winsta0\Default` normally, `Winlogon` while the machine is locked / at
//! the login screen / showing a UAC prompt. Both DXGI Desktop Duplication and
//! `SendInput` operate on the calling thread's desktop, so a capture or
//! injection thread that follows the input desktop is exactly what lets the
//! stream see and drive the secure desktop an in-session app is walled off
//! from.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

/// True only in the SYSTEM service worker (flagged by the env var the service
/// sets when it launches the worker). Gates every secure-desktop behavior —
/// desktop-following capture/inject — so the normal in-app host is untouched.
/// Read once.
pub fn secure_desktop_worker() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("STREAMBEAM_SECURE_DESKTOP").is_ok())
}

use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK, OpenInputDesktop,
    SetThreadDesktop,
};

// GENERIC_ALL — capture needs read, SendInput needs write; ask for both.
const DESKTOP_GENERIC_ALL: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(0x1000_0000);

thread_local! {
    /// Per-thread follower — each thread that tracks the input desktop keeps
    /// its own last-attached HDESK. Access via [`input_desktop`].
    static FOLLOWER: Follower = const { Follower::new() };
}

/// This thread's input-desktop follower (thread-local). Call `.follow()` on it
/// before capture/injection and again on any `ACCESS_LOST`-style failure.
pub fn input_desktop() -> &'static Follower {
    // The thread_local owns the Follower for the thread's lifetime; hand out a
    // 'static ref valid on the owning thread (the value never moves or drops
    // while the thread runs, and callers only use it on that thread).
    FOLLOWER.with(|f| unsafe { &*(f as *const Follower) })
}

/// The HDESK this thread last switched to, so we can close the previous one
/// only after a successful switch (closing the active thread desktop is
/// illegal).
pub struct Follower {
    current: AtomicIsize,
}

impl Follower {
    pub const fn new() -> Self {
        Self {
            current: AtomicIsize::new(0),
        }
    }

    /// Point this thread at the current input desktop. Returns `true` if the
    /// thread is now on the input desktop (including when it already was).
    /// Best-effort: on failure the thread simply stays where it is, and the
    /// caller's existing `ACCESS_LOST`/retry path handles the fallout.
    pub fn follow(&self) -> bool {
        // Only the SYSTEM service worker follows desktops — in a normal
        // logged-in app the injector/capture threads are already on the one
        // interactive desktop, and switching to a fresh HDESK of it breaks
        // `SendInput` (silent dead input, seen 2026-07-19). The service sets
        // `STREAMBEAM_SECURE_DESKTOP=1` when it launches the worker; without
        // it this is a no-op that reports success.
        if !secure_desktop_worker() {
            return true;
        }
        unsafe {
            // `OpenInputDesktop` needs no station switch for a SYSTEM process
            // already in the console session; flags 0, non-inheritable.
            let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_GENERIC_ALL)
            else {
                return false;
            };
            if SetThreadDesktop(desk).is_err() {
                // Common, benign reason: the thread owns a window/hook. Our
                // capture and injector threads deliberately don't, but close
                // the handle and report no-switch regardless.
                let _ = CloseDesktop(desk);
                return false;
            }
            // Switch succeeded — retire the previous desktop handle now that
            // it's no longer the thread's desktop.
            let prev = self.current.swap(desk.0 as isize, Ordering::Relaxed);
            if prev != 0 {
                let _ = CloseDesktop(HDESK(prev as *mut core::ffi::c_void));
            }
            true
        }
    }
}
