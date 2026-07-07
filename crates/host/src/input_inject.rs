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
            InputEvent::Key { vk, down } => {
                key(vk, down);
            }
            InputEvent::GamepadPlug { .. } | InputEvent::GamepadState { .. } => {
                crate::gamepad_inject::apply(event);
            }
            _ => {}
        }
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
    let _ = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
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

#[cfg(windows)]
unsafe fn key(vk: u16, down: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let _ = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
}
