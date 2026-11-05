//! OS clipboard bridge for the headless host binary.
//!
//! The wire half of text clipboard sync lives in [`HostControls`]: client
//! updates land in `take_remote_clipboard`, and `publish_clipboard` fans host
//! text out to the viewer. When the Flutter app is the host, its Dart shell
//! polls the platform clipboard and drives those two calls. The standalone
//! `streambeam-host.exe`, which is what `streambeam-svc` runs on a real
//! Windows box, has no Dart shell, so nothing ever touched the Windows
//! clipboard and sync silently did nothing in either direction. This module
//! is that missing poller.
//!
//! The sync logic ([`ClipboardBridge`]) is platform-neutral and tested here
//! against a fake; only [`SystemClipboard`] has a Windows implementation.

use std::sync::Arc;
use std::time::Duration;

use crate::server::HostControls;

/// Same cadence as the app shell's clipboard poll.
pub const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// Plain-text access to the machine's clipboard.
pub trait SystemClipboard {
    /// A counter that changes whenever the clipboard does, so an unchanged
    /// clipboard costs no read. `None` means "unknown, read every tick".
    fn change_count(&mut self) -> Option<u64> {
        None
    }
    /// Current text, or `None` if it holds no text or can't be opened.
    fn read_text(&mut self) -> Option<String>;
    /// Replace the clipboard with `text`. `false` means try again later
    /// (another process may hold the clipboard open).
    fn write_text(&mut self, text: &str) -> bool;
}

/// One side of the two-way sync: applies viewer text locally and publishes
/// local changes, de-duplicating against the last value either way so a
/// value never echoes back to where it came from.
pub struct ClipboardBridge<C> {
    clipboard: C,
    last_synced: Option<String>,
    last_change: Option<u64>,
    pending_write: Option<String>,
}

impl<C: SystemClipboard> ClipboardBridge<C> {
    pub fn new(clipboard: C) -> Self {
        Self {
            clipboard,
            last_synced: None,
            last_change: None,
            pending_write: None,
        }
    }

    /// One poll. `active` is false when nobody is watching or the host is on
    /// a desktop whose clipboard must not leave the machine (lock screen);
    /// then the clipboard is neither read nor written. A viewer update that
    /// arrives meanwhile is kept and applied once sync is active again.
    pub fn tick(&mut self, controls: &HostControls, active: bool) {
        if let Some(remote) = controls.take_remote_clipboard() {
            self.pending_write = Some(remote);
        }
        if !active {
            return;
        }

        if let Some(remote) = self.pending_write.take() {
            if self.last_synced.as_deref() == Some(remote.as_str()) {
                // Already what we have; nothing to do.
            } else if self.clipboard.write_text(&remote) {
                self.last_synced = Some(remote);
                // Our own write bumps the change counter; skip re-reading it.
                self.last_change = self.clipboard.change_count();
                return;
            } else {
                self.pending_write = Some(remote);
                return;
            }
        }

        let change = self.clipboard.change_count();
        if change.is_some() && change == self.last_change {
            return;
        }
        let Some(text) = self.clipboard.read_text() else {
            // Couldn't open it: leave `last_change` so the next tick retries.
            return;
        };
        self.last_change = change;
        if text.is_empty() || self.last_synced.as_deref() == Some(text.as_str()) {
            return;
        }
        self.last_synced = Some(text.clone());
        controls.publish_clipboard(text);
    }
}

/// Run the bridge for the life of the process on its own thread. Windows-only;
/// elsewhere the headless binary is a dev tool and the app shell owns sync.
pub fn spawn(controls: Arc<HostControls>) {
    #[cfg(windows)]
    {
        let spawned = std::thread::Builder::new()
            .name("streambeam-clipboard".into())
            .spawn(move || {
                let mut bridge = ClipboardBridge::new(windows_impl::WinClipboard);
                loop {
                    let active =
                        !controls.viewers().is_empty() && windows_impl::clipboard_may_sync();
                    bridge.tick(&controls, active);
                    std::thread::sleep(POLL_INTERVAL);
                }
            });
        if let Err(e) = spawned {
            log::warn!("clipboard sync disabled: {e}");
        }
    }
    #[cfg(not(windows))]
    drop(controls);
}

#[cfg(windows)]
mod windows_impl {
    #![allow(unsafe_op_in_unsafe_fn)]

    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    use super::SystemClipboard;

    /// The SYSTEM service worker follows the input desktop onto the lock
    /// screen. The clipboard is per window station, so it would still be
    /// reachable there; don't sync while locked, so a locked machine never
    /// hands out what was copied before it locked.
    pub fn clipboard_may_sync() -> bool {
        !crate::desktop_win::secure_desktop_worker()
            || crate::desktop_win::input_desktop_name().as_deref() == Some("Default")
    }

    pub struct WinClipboard;

    /// Holds the clipboard open; closes it on drop, including early returns.
    struct Open;

    impl Open {
        fn new() -> Option<Self> {
            // Null owner: fine for reading, and for writing as long as we
            // EmptyClipboard first (ownership then goes to no window).
            (unsafe { OpenClipboard(std::ptr::null_mut()) } != 0).then_some(Open)
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }

    const FORMAT: u32 = CF_UNICODETEXT as u32;

    impl SystemClipboard for WinClipboard {
        fn change_count(&mut self) -> Option<u64> {
            Some(unsafe { GetClipboardSequenceNumber() } as u64)
        }

        fn read_text(&mut self) -> Option<String> {
            unsafe {
                if IsClipboardFormatAvailable(FORMAT) == 0 {
                    // Non-text content (an image, files): nothing to sync, and
                    // not an error worth retrying.
                    return Some(String::new());
                }
                let _open = Open::new()?;
                let handle = GetClipboardData(FORMAT);
                if handle.is_null() {
                    return None;
                }
                let ptr = GlobalLock(handle) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                // Bounded by the allocation, not just the terminator, so a
                // malformed producer can't make us read past its buffer.
                let max = GlobalSize(handle) / 2;
                let slice = std::slice::from_raw_parts(ptr, max);
                let len = slice.iter().position(|&c| c == 0).unwrap_or(max);
                let text = String::from_utf16_lossy(&slice[..len]);
                GlobalUnlock(handle);
                Some(text)
            }
        }

        fn write_text(&mut self, text: &str) -> bool {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let Some(_open) = Open::new() else {
                    return false;
                };
                let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
                if mem.is_null() {
                    return false;
                }
                let dst = GlobalLock(mem) as *mut u16;
                if dst.is_null() {
                    GlobalFree(mem);
                    return false;
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                GlobalUnlock(mem);
                EmptyClipboard();
                if SetClipboardData(FORMAT, mem).is_null() {
                    // Ownership didn't transfer; the memory is still ours.
                    GlobalFree(mem);
                    return false;
                }
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        text: Option<String>,
        seq: u64,
        writes: usize,
        reads: usize,
        fail_writes: bool,
    }

    impl SystemClipboard for &mut Fake {
        fn change_count(&mut self) -> Option<u64> {
            Some(self.seq)
        }
        fn read_text(&mut self) -> Option<String> {
            self.reads += 1;
            Some(self.text.clone().unwrap_or_default())
        }
        fn write_text(&mut self, text: &str) -> bool {
            if self.fail_writes {
                return false;
            }
            self.writes += 1;
            self.seq += 1;
            self.text = Some(text.into());
            true
        }
    }

    fn copy(fake: &mut Fake, text: &str) {
        fake.seq += 1;
        fake.text = Some(text.into());
    }

    fn published(controls: &HostControls) -> (u64, String) {
        controls.clipboard_to_client.borrow().clone()
    }

    #[test]
    fn local_copy_is_published_once() {
        let controls = HostControls::new();
        let mut fake = Fake::default();
        copy(&mut fake, "host text");
        let mut bridge = ClipboardBridge::new(&mut fake);

        bridge.tick(&controls, true);
        bridge.tick(&controls, true);

        assert_eq!(published(&controls), (1, "host text".into()));
        drop(bridge);
        assert_eq!(fake.reads, 1, "unchanged sequence must not re-read");
    }

    #[test]
    fn viewer_text_is_applied_and_not_echoed_back() {
        let controls = HostControls::new();
        let mut fake = Fake::default();
        let mut bridge = ClipboardBridge::new(&mut fake);

        controls.receive_remote_clipboard("from phone".into());
        bridge.tick(&controls, true);
        bridge.tick(&controls, true);

        assert_eq!(published(&controls).0, 0, "must not echo to the viewer");
        drop(bridge);
        assert_eq!(fake.text.as_deref(), Some("from phone"));
        assert_eq!(fake.writes, 1);
    }

    #[test]
    fn nothing_moves_while_inactive_and_pending_text_lands_later() {
        let controls = HostControls::new();
        let mut fake = Fake::default();
        copy(&mut fake, "secret before lock");
        let mut bridge = ClipboardBridge::new(&mut fake);

        controls.receive_remote_clipboard("typed while locked".into());
        bridge.tick(&controls, false);
        assert_eq!(published(&controls).0, 0);

        bridge.tick(&controls, true);
        drop(bridge);
        assert_eq!(fake.text.as_deref(), Some("typed while locked"));
        assert_eq!(published(&controls).0, 0);
    }

    #[test]
    fn failed_write_is_retried() {
        let controls = HostControls::new();
        let mut fake = Fake {
            fail_writes: true,
            ..Fake::default()
        };
        let mut bridge = ClipboardBridge::new(&mut fake);

        controls.receive_remote_clipboard("retry me".into());
        bridge.tick(&controls, true);
        assert_eq!(bridge.clipboard.writes, 0);
        bridge.clipboard.fail_writes = false;
        bridge.tick(&controls, true);

        drop(bridge);
        assert_eq!(fake.text.as_deref(), Some("retry me"));
    }

    #[test]
    fn host_change_after_viewer_text_is_published() {
        let controls = HostControls::new();
        let mut fake = Fake::default();
        let mut bridge = ClipboardBridge::new(&mut fake);

        controls.receive_remote_clipboard("from phone".into());
        bridge.tick(&controls, true);
        copy(bridge.clipboard, "edited on host");
        bridge.tick(&controls, true);

        assert_eq!(published(&controls), (1, "edited on host".into()));
    }

    #[test]
    fn empty_or_non_text_clipboard_is_not_published() {
        let controls = HostControls::new();
        let mut fake = Fake::default();
        copy(&mut fake, "");
        let mut bridge = ClipboardBridge::new(&mut fake);
        bridge.tick(&controls, true);
        assert_eq!(published(&controls).0, 0);
    }
}
