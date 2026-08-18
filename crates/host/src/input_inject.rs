// This module is Win32 FFI glue; each `unsafe fn` body is deliberately one big
// unsafe region rather than nesting `unsafe {}` around every call.
#![cfg_attr(windows, allow(unsafe_op_in_unsafe_fn))]

//! Apply client [`InputEvent`]s to the host OS (Windows `SendInput` / macOS CGEvent).

use streambeam_proto::input::InputEvent;

/// Sparse input tracing, so "the key never arrived", "Windows refused it" and
/// "the game ignored it" stop looking identical from the outside.
///
/// The host logged only `input stream opened` / `input stream ended`, which
/// means a keyboard that does nothing had *no* diagnostic surface at all — the
/// same blind spot that made the black-screen class so expensive. First few of
/// everything, then every Nth, so a live session stays readable.
pub mod diag {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// One counter per event kind. A single shared counter looked fine and was
    /// useless: mouse-move events arrive hundreds of times a second, so they
    /// consumed the "first five" and every "every-500th" slot, and a keypress —
    /// the rare event you actually want to see — would only ever be logged if
    /// it happened to land exactly on a multiple of 500. The first run of this
    /// tracing showed zero key lines and could not distinguish "no keys were
    /// sent" from "keys were sent and never sampled".
    #[derive(Clone, Copy)]
    pub enum Kind {
        Key,
        MouseMove,
        MouseAbs,
        MouseButton,
        Scroll,
        Touch,
        Clipboard,
        Gamepad,
    }

    const KINDS: usize = 8;
    static RECEIVED: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];
    /// Only the Windows injector can be refused; macOS/Android take other paths.
    #[cfg(windows)]
    static REFUSED: AtomicU64 = AtomicU64::new(0);

    fn sparse(n: u64, every: u64) -> bool {
        n <= 5 || n % every == 0
    }

    /// How often to log each kind once past the first few — high-rate motion
    /// is sampled coarsely, discrete events (keys, buttons) finely, so a
    /// keypress is always visible without motion flooding the log.
    fn every(kind: Kind) -> u64 {
        match kind {
            Kind::MouseMove | Kind::MouseAbs | Kind::Touch | Kind::Gamepad => 500,
            Kind::Key | Kind::MouseButton | Kind::Scroll | Kind::Clipboard => 20,
        }
    }

    pub fn note_received(kind: Kind, what: &str) {
        let n = RECEIVED[kind as usize].fetch_add(1, Ordering::Relaxed) + 1;
        if sparse(n, every(kind)) {
            crate::logging::log(&format!("input: received {what} (#{n} of this kind)"));
        }
    }

    /// `SendInput` returns how many events it actually inserted; the call sites
    /// used to `let _ =` that. Zero means Windows refused the injection —
    /// usually UIPI, when the foreground window runs at a higher integrity
    /// level than the host.
    #[cfg(windows)]
    pub fn note_injected(what: &str, inserted: u32, err: u32) {
        if inserted >= 1 {
            return;
        }
        let n = REFUSED.fetch_add(1, Ordering::Relaxed) + 1;
        if sparse(n, 200) {
            crate::logging::log(&format!(
                "input: SendInput refused {what} — GetLastError {err} \
                 (#{n} refused; error 5 = access denied, i.e. the foreground \
                 window outranks this process)"
            ));
        }
    }
}

/// Short label for the trace. Deliberately excludes clipboard *contents* —
/// host.log must not become a keylogger of pasted text.
fn describe(event: &InputEvent) -> (diag::Kind, String) {
    use diag::Kind;
    match event {
        InputEvent::Key { vk, down } => (
            Kind::Key,
            format!("key vk={vk} {}", if *down { "down" } else { "up" }),
        ),
        InputEvent::MouseButton { button, down } => (
            Kind::MouseButton,
            format!(
                "mouse button {button} {}",
                if *down { "down" } else { "up" }
            ),
        ),
        InputEvent::MouseMove { .. } => (Kind::MouseMove, "mouse move".to_string()),
        InputEvent::MouseAbs { .. } => (Kind::MouseAbs, "mouse abs".to_string()),
        InputEvent::Scroll { .. } => (Kind::Scroll, "scroll".to_string()),
        InputEvent::TouchFrame { contacts, .. } => (
            Kind::Touch,
            format!("touch frame ({} contacts)", contacts.len()),
        ),
        InputEvent::ClipboardText { text } => (
            Kind::Clipboard,
            format!("clipboard text ({} bytes)", text.len()),
        ),
        InputEvent::GamepadState { pad, .. } => (Kind::Gamepad, format!("gamepad {pad} state")),
        InputEvent::SecureAttention => (Kind::Key, "Ctrl+Alt+Del (SAS)".to_string()),
        InputEvent::GamepadPlug { pad, connected } => (
            Kind::Gamepad,
            format!(
                "gamepad {pad} {}",
                if *connected { "plugged" } else { "unplugged" }
            ),
        ),
    }
}

pub fn apply(event: InputEvent) {
    let (kind, what) = describe(&event);
    diag::note_received(kind, &what);
    #[cfg(windows)]
    {
        apply_windows(event);
        return;
    }
    #[cfg(target_os = "macos")]
    {
        apply_macos(event);
        return;
    }
    #[cfg(target_os = "android")]
    {
        crate::android_input::submit(event);
        return;
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    let _ = event;
}

#[cfg(target_os = "macos")]
fn apply_macos(event: InputEvent) {
    use core_graphics2::{
        event::{CGEvent, CGEventSource},
        event_types::{
            CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton, CGScrollEventUnit,
        },
        geometry::CGPoint,
    };
    use std::sync::{LazyLock, Mutex};

    static BUTTONS: LazyLock<Mutex<u8>> = LazyLock::new(|| Mutex::new(0));
    static SCROLL_REMAINDER: LazyLock<Mutex<(i32, i32)>> = LazyLock::new(|| Mutex::new((0, 0)));

    fn source() -> Option<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
    }

    fn cursor_location() -> Option<CGPoint> {
        CGEvent::new(source()?).map(|event| event.get_location())
    }

    fn post_mouse(event_type: CGEventType, point: CGPoint, button: CGMouseButton) {
        if let Some(event) =
            source().and_then(|source| CGEvent::new_mouse_event(source, event_type, point, button))
        {
            event.post(CGEventTapLocation::HIDEventTap);
        }
    }

    fn motion_type(buttons: u8) -> (CGEventType, CGMouseButton) {
        if buttons & 1 != 0 {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if buttons & 2 != 0 {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if buttons & 4 != 0 {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        }
    }

    match event {
        InputEvent::MouseAbs { x, y, ref_w, ref_h } => {
            let (origin_x, origin_y, width, height) =
                crate::macos_screen::selected_display_bounds();
            let point = CGPoint::new(
                origin_x + f64::from(x) * width / f64::from(ref_w.max(1)),
                origin_y + f64::from(y) * height / f64::from(ref_h.max(1)),
            );
            let buttons = *BUTTONS.lock().expect("mouse button state poisoned");
            let (event_type, button) = motion_type(buttons);
            post_mouse(event_type, point, button);
        }
        InputEvent::MouseMove { dx, dy } => {
            if dx == 0 && dy == 0 {
                return;
            }
            if let Some(mut point) = cursor_location() {
                point.x += f64::from(dx);
                point.y += f64::from(dy);
                let buttons = *BUTTONS.lock().expect("mouse button state poisoned");
                let (event_type, button) = motion_type(buttons);
                post_mouse(event_type, point, button);
            }
        }
        InputEvent::MouseButton { button, down } => {
            let Some((mask, mouse_button, down_type, up_type)) = (match button {
                0 => Some((
                    1,
                    CGMouseButton::Left,
                    CGEventType::LeftMouseDown,
                    CGEventType::LeftMouseUp,
                )),
                1 => Some((
                    2,
                    CGMouseButton::Right,
                    CGEventType::RightMouseDown,
                    CGEventType::RightMouseUp,
                )),
                2 => Some((
                    4,
                    CGMouseButton::Center,
                    CGEventType::OtherMouseDown,
                    CGEventType::OtherMouseUp,
                )),
                _ => None,
            }) else {
                return;
            };
            let mut buttons = BUTTONS.lock().expect("mouse button state poisoned");
            if down {
                *buttons |= mask;
            } else {
                *buttons &= !mask;
            }
            drop(buttons);
            if let Some(point) = cursor_location() {
                post_mouse(if down { down_type } else { up_type }, point, mouse_button);
            }
        }
        InputEvent::Scroll { dx, dy } => {
            // The client uses three wire units per source pixel (120 per classic
            // wheel notch). Preserve sub-pixel remainders across events.
            let mut remainder = SCROLL_REMAINDER.lock().expect("scroll state poisoned");
            remainder.0 += i32::from(dx);
            remainder.1 += i32::from(dy);
            let pixels_x = remainder.0 / 3;
            let pixels_y = remainder.1 / 3;
            remainder.0 %= 3;
            remainder.1 %= 3;
            drop(remainder);
            if (pixels_x != 0 || pixels_y != 0)
                && let Some(event) = source().and_then(|source| {
                    CGEvent::new_scroll_event(
                        source,
                        CGScrollEventUnit::Pixel,
                        2,
                        pixels_y,
                        pixels_x,
                        0,
                    )
                })
            {
                event.post(CGEventTapLocation::HIDEventTap);
            }
        }
        InputEvent::Key { vk, down } => {
            if let Some(keycode) = mac_keycode(vk)
                && let Some(event) =
                    source().and_then(|source| CGEvent::new_keyboard_event(source, keycode, down))
            {
                event.post(CGEventTapLocation::HIDEventTap);
            }
        }
        InputEvent::TouchFrame { .. }
        | InputEvent::GamepadPlug { .. }
        | InputEvent::GamepadState { .. }
        | InputEvent::SecureAttention
        | InputEvent::ClipboardText { .. } => {}
    }
}

/// Translate the protocol's Windows-VK key identifiers to macOS virtual
/// keycodes (ANSI positions). This intentionally describes physical keys;
/// macOS applies the user's active keyboard layout when producing text.
#[cfg(target_os = "macos")]
fn mac_keycode(vk: u16) -> Option<u16> {
    Some(match vk {
        0x41 => 0,
        0x53 => 1,
        0x44 => 2,
        0x46 => 3,
        0x48 => 4,
        0x47 => 5,
        0x5A => 6,
        0x58 => 7,
        0x43 => 8,
        0x56 => 9,
        0x42 => 11,
        0x51 => 12,
        0x57 => 13,
        0x45 => 14,
        0x52 => 15,
        0x59 => 16,
        0x54 => 17,
        0x31 => 18,
        0x32 => 19,
        0x33 => 20,
        0x34 => 21,
        0x36 => 22,
        0x35 => 23,
        0xBB => 24,
        0x39 => 25,
        0x37 => 26,
        0xBD => 27,
        0x38 => 28,
        0x30 => 29,
        0xDD => 30,
        0x4F => 31,
        0x55 => 32,
        0xDB => 33,
        0x49 => 34,
        0x50 => 35,
        0x0D => 36,
        0x4C => 37,
        0x4A => 38,
        0xDE => 39,
        0x4B => 40,
        0xBA => 41,
        0xDC => 42,
        0xBC => 43,
        0xBF => 44,
        0x4E => 45,
        0x4D => 46,
        0xBE => 47,
        0x09 => 48,
        0x20 => 49,
        0xC0 => 50,
        0x08 => 51,
        0x1B => 53,
        0x5C => 54,
        0x5B => 55,
        0xA0 | 0x10 => 56,
        0x14 => 57,
        0xA4 | 0x12 => 58,
        0xA2 | 0x11 => 59,
        0xA1 => 60,
        0xA5 => 61,
        0xA3 => 62,
        0x6E => 65,
        0x6A => 67,
        0x6B => 69,
        0x6F => 75,
        0x6D => 78,
        0x6C => 81,
        0x60 => 82,
        0x61 => 83,
        0x62 => 84,
        0x63 => 85,
        0x64 => 86,
        0x65 => 87,
        0x66 => 88,
        0x67 => 89,
        0x68 => 91,
        0x69 => 92,
        0x74 => 96,
        0x75 => 97,
        0x76 => 98,
        0x72 => 99,
        0x77 => 100,
        0x78 => 101,
        0x7A => 109,
        0x7B => 103,
        0x7C => 111,
        0x24 => 115,
        0x21 => 116,
        0x2E => 117,
        0x73 => 118,
        0x23 => 119,
        0x71 => 120,
        0x22 => 121,
        0x70 => 122,
        0x25 => 123,
        0x27 => 124,
        0x28 => 125,
        0x26 => 126,
        _ => return None,
    })
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::mac_keycode;

    #[test]
    fn maps_vk_letters_modifiers_and_navigation() {
        assert_eq!(mac_keycode(0x41), Some(0));
        assert_eq!(mac_keycode(0xA0), Some(56));
        assert_eq!(mac_keycode(0x0D), Some(36));
        assert_eq!(mac_keycode(0x25), Some(123));
        assert_eq!(mac_keycode(0xFFFF), None);
    }
}

/// Route Windows input through a single dedicated injector thread that stays
/// attached to the current *input* desktop. `SendInput`/`SetCursorPos` act on
/// the calling thread's desktop, so injecting from tokio worker threads (which
/// live on whatever desktop they were spawned on) can never reach the
/// lock/login/UAC desktop the SYSTEM worker is capturing. The dedicated thread
/// re-follows the input desktop before each event — cheap, and it means input
/// tracks a lock/unlock/UAC transition with no extra plumbing. In a normal
/// user session the follow is a no-op and this behaves exactly as before,
/// just one hop off the caller's thread.
#[cfg(windows)]
fn apply_windows(event: InputEvent) {
    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};

    // Normal in-app host: inject directly on the calling thread, exactly as
    // before the secure-desktop work — that thread is already on the one
    // interactive desktop, and the dedicated-thread + desktop-follow routing
    // only exists for the SYSTEM service worker. Routing normal input through
    // a spawned thread was a needless behavior change that broke injection
    // (dead mouse/keys, 2026-07-19), so it's now strictly opt-in.
    if !crate::desktop_win::secure_desktop_worker() {
        inject_windows(event);
        return;
    }

    // Service worker only: a single dedicated thread that re-attaches to the
    // current input desktop before each event, so SendInput reaches the
    // lock/login/UAC desktop (tokio workers can't switch desktops safely).
    static INJECTOR: OnceLock<Sender<InputEvent>> = OnceLock::new();
    let tx = INJECTOR.get_or_init(|| {
        let (tx, rx) = channel::<InputEvent>();
        std::thread::Builder::new()
            .name("streambeam-input".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    crate::desktop_win::input_desktop().follow();
                    inject_windows(event);
                }
            })
            .expect("spawn input injector thread");
        tx
    });
    let _ = tx.send(event);
}

#[cfg(windows)]
fn inject_windows(event: InputEvent) {
    use streambeam_proto::input::InputEvent;

    unsafe {
        match event {
            InputEvent::MouseAbs { x, y, ref_w, ref_h } => {
                set_cursor_abs(x, y, ref_w, ref_h);
            }
            InputEvent::MouseButton { button, down } => {
                mouse_button(button, down);
            }
            InputEvent::MouseMove { dx, dy } => {
                mouse_move(dx, dy);
            }
            InputEvent::Scroll { dx, dy } => {
                scroll_wheel(dx, dy);
            }
            InputEvent::Key { vk, down } => {
                key(vk, down);
            }
            InputEvent::GamepadPlug { .. } | InputEvent::GamepadState { .. } => {
                crate::gamepad_inject::apply(event);
            }
            InputEvent::SecureAttention => {
                send_sas();
            }
            InputEvent::ClipboardText { .. } => {}
            InputEvent::TouchFrame { .. } => {}
            _ => {}
        }
    }
}

/// Raise Ctrl+Alt+Del.
///
/// `SendInput` categorically cannot do this — the secure attention sequence is
/// recognised by the kernel precisely so that no synthesised input can forge
/// it, which is what stops a program faking the login screen. `SendSAS` is the
/// only way, it lives in `sas.dll`, and it works only for a caller running as
/// SYSTEM (or with the `SoftwareSASGeneration` policy allowing services). That
/// is the secure-desktop service worker; from the ordinary in-session host this
/// is expected to fail, so it says so rather than appearing to work.
///
/// Loaded dynamically rather than linked: `sas.dll` is present on desktop
/// Windows but this binary also has to run on machines where nothing about the
/// secure desktop is in play, and a missing import would stop it starting.
#[cfg(windows)]
fn send_sas() {
    // `libloading` rather than raw LoadLibrary: this crate already loads
    // nvEncodeAPI64.dll that way (see nvenc.rs), and it keeps `windows-sys`'s
    // feature list from growing for one call.
    type SendSasFn = unsafe extern "system" fn(i32);
    let result = unsafe {
        libloading::Library::new("sas.dll").and_then(|lib| {
            let f: libloading::Symbol<SendSasFn> = lib.get(b"SendSAS")?;
            // FALSE = "as services", the documented value for a SYSTEM caller.
            f(0);
            Ok(())
        })
    };
    match result {
        Ok(()) => crate::logging::log(&format!(
            "input: sent Ctrl+Alt+Del (SendSAS){}",
            if crate::desktop_win::secure_desktop_worker() {
                ""
            } else {
                " — from the in-session host, which Windows normally refuses; \
                 enable the secure-desktop service for this to work"
            }
        )),
        Err(e) => crate::logging::log(&format!("input: Ctrl+Alt+Del failed — {e}")),
    }
}

#[cfg(windows)]
unsafe fn set_cursor_abs(x: u16, y: u16, ref_w: u16, ref_h: u16) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos,
    };

    let rw = ref_w.max(1) as i32;
    let rh = ref_h.max(1) as i32;
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let px = i32::from(x).saturating_mul(sw) / rw;
    let py = i32::from(y).saturating_mul(sh) / rh;
    SetCursorPos(px, py);
}

#[cfg(windows)]
unsafe fn mouse_move(dx: i16, dy: i16) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };

    if dx == 0 && dy == 0 {
        return;
    }

    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: i32::from(dx),
                dy: i32::from(dy),
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inserted = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    diag::note_injected(
        "mouse move",
        inserted,
        windows_sys::Win32::Foundation::GetLastError(),
    );
}

#[cfg(windows)]
unsafe fn mouse_button(button: u8, down: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
        mouse_event,
    };
    const XBUTTON1: u32 = 1; // back
    const XBUTTON2: u32 = 2; // forward

    let (flags, data) = match (button, down) {
        (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (0, false) => (MOUSEEVENTF_LEFTUP, 0),
        (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        _ => return,
    };
    mouse_event(flags, 0, 0, data as i32, 0);
}

#[cfg(windows)]
unsafe fn scroll_wheel(dx: i16, dy: i16) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, mouse_event,
    };

    // Wire deltas are wheel units (120 = one notch; see PROTOCOL.md), so
    // fractional-notch trackpad scrolling stays smooth end-to-end.
    if dy != 0 {
        mouse_event(MOUSEEVENTF_WHEEL, 0, 0, i32::from(dy), 0);
    }
    if dx != 0 {
        mouse_event(MOUSEEVENTF_HWHEEL, 0, 0, i32::from(dx), 0);
    }
}

/// Keys Windows marks with the extended-key bit. Without it the injected
/// scancode collides with the numpad equivalent (arrows vs numpad digits being
/// the classic case), so games read the wrong key.
#[cfg(windows)]
fn is_extended_key(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_APPS, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN,
        VK_NEXT, VK_NUMLOCK, VK_PRIOR, VK_RCONTROL, VK_RIGHT, VK_RMENU, VK_RWIN, VK_SNAPSHOT,
        VK_UP,
    };
    matches!(
        vk,
        VK_RMENU
            | VK_RCONTROL
            | VK_INSERT
            | VK_DELETE
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_NUMLOCK
            | VK_SNAPSHOT
            | VK_DIVIDE
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
    )
}

#[cfg(windows)]
unsafe fn key(vk: u16, down: bool) {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput,
    };

    // Fill the scancode rather than leaving it 0. Anything reading the keyboard
    // through DirectInput or Raw Input — which is most games — looks at the
    // scancode, not the virtual key, and a zero scancode reads as "no key".
    // Desktop apps were unaffected, which is why this went unnoticed: typing
    // into Notepad over the stream works fine either way.
    let scan = MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) as u16;
    let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inserted = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    diag::note_injected(
        &format!(
            "key vk={vk} scan={scan} {}",
            if down { "down" } else { "up" }
        ),
        inserted,
        GetLastError(),
    );
}
