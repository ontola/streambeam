//! Input events on stream S1 (client → host).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
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
}
