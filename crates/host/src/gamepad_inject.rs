//! Virtual Xbox 360 controllers via ViGEmBus (Windows).

use streambeam_proto::input::InputEvent;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};

    struct PadSlot {
        target: Option<Xbox360Wired<Client>>,
    }

    struct GamepadManager {
        vigem_ok: bool,
        warned_missing: bool,
        pads: [PadSlot; 4],
    }

    impl GamepadManager {
        fn new() -> Self {
            let vigem_ok = Client::connect().is_ok();
            Self {
                vigem_ok,
                warned_missing: false,
                pads: std::array::from_fn(|_| PadSlot { target: None }),
            }
        }

        fn warn_missing(&mut self) {
            if self.warned_missing {
                return;
            }
            self.warned_missing = true;
            tracing::warn!(
                "ViGEmBus is not installed — Bluetooth gamepad input is disabled. \
                 Install from https://github.com/ViGEm/ViGEmBus/releases (mouse/touch still work)."
            );
        }

        fn plug(&mut self, pad: u8, connected: bool) {
            let Some(slot) = self.pads.get_mut(pad as usize) else {
                return;
            };
            if connected {
                if slot.target.is_some() {
                    return;
                }
                if !self.vigem_ok {
                    self.warn_missing();
                    return;
                }
                let Ok(client) = Client::connect() else {
                    self.vigem_ok = false;
                    self.warn_missing();
                    return;
                };
                let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
                if target.plugin().is_err() || target.wait_ready().is_err() {
                    tracing::warn!("ViGEm: failed to plug virtual pad {pad}");
                    return;
                }
                tracing::info!("ViGEm: virtual Xbox 360 pad {pad} connected");
                slot.target = Some(target);
            } else if let Some(target) = slot.target.take() {
                let _ = target.unplug();
                tracing::info!("ViGEm: virtual Xbox 360 pad {pad} disconnected");
            }
        }

        fn state(
            &mut self,
            pad: u8,
            buttons: u16,
            lt: u8,
            rt: u8,
            lx: i16,
            ly: i16,
            rx: i16,
            ry: i16,
        ) {
            if self.pads.get(pad as usize).is_none() {
                return;
            }
            if self.pads[pad as usize].target.is_none() {
                self.plug(pad, true);
            }
            let Some(target) = self.pads[pad as usize].target.as_mut() else {
                return;
            };
            let report = XGamepad {
                buttons: XButtons::from(buttons),
                left_trigger: lt,
                right_trigger: rt,
                thumb_lx: lx,
                thumb_ly: ly,
                thumb_rx: rx,
                thumb_ry: ry,
            };
            if let Err(e) = target.update(&report) {
                tracing::warn!("ViGEm pad {pad} update failed: {e:?}");
            }
        }
    }

    static MGR: OnceLock<Mutex<GamepadManager>> = OnceLock::new();

    fn mgr() -> &'static Mutex<GamepadManager> {
        MGR.get_or_init(|| Mutex::new(GamepadManager::new()))
    }

    pub fn apply(event: InputEvent) {
        let mut guard = mgr().lock().unwrap();
        match event {
            InputEvent::GamepadPlug { pad, connected } => guard.plug(pad, connected),
            InputEvent::GamepadState {
                pad,
                buttons,
                lt,
                rt,
                lx,
                ly,
                rx,
                ry,
            } => guard.state(pad, buttons, lt, rt, lx, ly, rx, ry),
            _ => {}
        }
    }

    pub fn reset() {
        for pad in 0..4u8 {
            mgr().lock().unwrap().plug(pad, false);
        }
    }
}

#[cfg(windows)]
pub use imp::{apply, reset};

#[cfg(not(windows))]
pub fn apply(_event: InputEvent) {}

#[cfg(not(windows))]
pub fn reset() {}
