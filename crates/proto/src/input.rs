//! Input events on stream S1 (client → host).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchContact {
    /// Stable only for the lifetime of this contact.
    pub id: u8,
    pub x: u16,
    pub y: u16,
    /// 0..=65535, with 65535 used when the viewer has no pressure sensor.
    pub pressure: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Full current touch state. An empty list releases every contact. Full
    /// state makes move coalescing and packet recovery unable to stick a
    /// finger down indefinitely.
    TouchFrame {
        contacts: Vec<TouchContact>,
        ref_w: u16,
        ref_h: u16,
    },
    MouseMove {
        dx: i16,
        dy: i16,
    },
    MouseAbs {
        x: u16,
        y: u16,
        ref_w: u16,
        ref_h: u16,
    },
    MouseButton {
        button: u8,
        down: bool,
    },
    Scroll {
        dx: i16,
        dy: i16,
    },
    /// UTF-8 text clipboard update from the viewing client.
    ClipboardText {
        text: String,
    },
    Key {
        vk: u16,
        down: bool,
    },
    GamepadState {
        pad: u8,
        buttons: u16,
        lt: u8,
        rt: u8,
        lx: i16,
        ly: i16,
        rx: i16,
        ry: i16,
    },
    GamepadPlug {
        pad: u8,
        connected: bool,
    },
    /// Ctrl+Alt+Del on Windows — the "secure attention sequence".
    ///
    /// A separate event rather than three `Key`s because SAS cannot be
    /// synthesised with `SendInput` at all; that is the entire point of it.
    /// Only `SendSAS` from `sas.dll` can raise it, and only from a process
    /// running as SYSTEM (or with the `SoftwareSASGeneration` policy set) —
    /// i.e. the secure-desktop service worker. Sending it as key events would
    /// look like it worked and do nothing.
    SecureAttention,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame;

    #[test]
    fn roundtrip() {
        let ev = InputEvent::GamepadState {
            pad: 0,
            buttons: 0x0f,
            lt: 10,
            rt: 20,
            lx: -100,
            ly: 50,
            rx: 0,
            ry: 0,
        };
        let bytes = frame::encode_frame(&ev).unwrap();
        let (decoded, _) = frame::decode_frame::<InputEvent>(&bytes).unwrap();
        assert_eq!(decoded, ev);
    }

    #[test]
    fn clipboard_roundtrip() {
        let event = InputEvent::ClipboardText {
            text: "hello from phone 👋".into(),
        };
        let bytes = frame::encode_frame(&event).unwrap();
        let (decoded, _) = frame::decode_frame::<InputEvent>(&bytes).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn multitouch_roundtrip() {
        let event = InputEvent::TouchFrame {
            contacts: vec![
                TouchContact {
                    id: 1,
                    x: 100,
                    y: 200,
                    pressure: 65_535,
                },
                TouchContact {
                    id: 2,
                    x: 300,
                    y: 400,
                    pressure: 32_768,
                },
            ],
            ref_w: 1920,
            ref_h: 1080,
        };
        let bytes = frame::encode_frame(&event).unwrap();
        let (decoded, _) = frame::decode_frame::<InputEvent>(&bytes).unwrap();
        assert_eq!(decoded, event);
    }
}
