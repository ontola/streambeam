// This module is Win32 FFI glue; each `unsafe fn` body is deliberately one big
// unsafe region rather than nesting `unsafe {}` around every call.
#![cfg_attr(windows, allow(unsafe_op_in_unsafe_fn))]

//! Apply client [`InputEvent`]s to the host OS (Windows [`SendInput`] / cursor APIs).

use streambeam_proto::input::InputEvent;

pub fn apply(event: InputEvent) {
    #[cfg(windows)]
    {
        apply_windows(event);
        return;
    }
    #[cfg(not(windows))]
    let _ = event;
}

#[cfg(windows)]
fn apply_windows(event: InputEvent) {
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
            _ => {}
        }
    }
}

#[cfg(windows)]
unsafe fn set_cursor_abs(x: u16, y: u16, ref_w: u16, ref_h: u16) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN};

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
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_MOVE};

    mouse_event(MOUSEEVENTF_MOVE, dx as i32, dy as i32, 0, 0);
}

#[cfg(windows)]
unsafe fn mouse_button(button: u8, down: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    };

    let flags = match (button, down) {
        (0, true) => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true) => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return,
    };
    mouse_event(flags, 0, 0, 0, 0);
}

#[cfg(windows)]
unsafe fn scroll_wheel(dx: i16, dy: i16) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL,
    };

    // Windows wheel delta is in multiples of 120; scale client lines roughly to that.
    if dy != 0 {
        let delta = i32::from(dy).saturating_mul(120);
        mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0);
    }
    if dx != 0 {
        let delta = i32::from(dx).saturating_mul(120);
        mouse_event(MOUSEEVENTF_HWHEEL, 0, 0, delta, 0);
    }
}
