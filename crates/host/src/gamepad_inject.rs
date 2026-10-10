//! Virtual Xbox 360 controllers via [VIIPER](https://github.com/Alia5/VIIPER) + USBIP.
//!
//! VIIPER runs device logic in userspace and presents real USB HID/XInput hardware
//! through the generic usbip-win2 driver (bundle once in the Windows installer,
//! Sunshine-style). Requires a local `viiper server` on port 3242.

use streambeam_proto::input::InputEvent;

#[cfg(windows)]
mod imp {
    use std::net::ToSocketAddrs;
    use std::sync::{Mutex, OnceLock};

    use super::InputEvent;
    use viiper_client::client::{DeviceStream, ViiperClient};
    use viiper_client::devices::xbox360::input::Xbox360Input;
    use viiper_client::types::{Device, DeviceCreateRequest};

    const API_ADDR: &str = "127.0.0.1:3242";

    struct PadSlot {
        bus_id: u32,
        dev_id: String,
        stream: Option<DeviceStream>,
    }

    struct GamepadManager {
        client: Option<ViiperClient>,
        bus_id: Option<u32>,
        warned_missing: bool,
        pads: [Option<PadSlot>; 4],
    }

    impl GamepadManager {
        fn new() -> Self {
            Self {
                client: None,
                bus_id: None,
                warned_missing: false,
                pads: std::array::from_fn(|_| None),
            }
        }

        fn ensure_client(&mut self) -> bool {
            if self.client.is_some() {
                return true;
            }
            let Ok(addr) = API_ADDR.to_socket_addrs().and_then(|mut i| {
                i.next()
                    .ok_or_else(|| std::io::Error::other("no VIIPER API address"))
            }) else {
                self.warn_missing("invalid VIIPER API address");
                return false;
            };
            let client = ViiperClient::new(addr);
            match client.ping() {
                Ok(_) => {
                    tracing::info!("VIIPER API reachable at {API_ADDR}");
                    self.client = Some(client);
                    true
                }
                Err(e) => {
                    self.warn_missing(&format!(
                        "VIIPER server not running ({e}). Start bundled viiper or run: viiper server"
                    ));
                    false
                }
            }
        }

        fn ensure_bus(&mut self) -> Option<u32> {
            if let Some(id) = self.bus_id {
                return Some(id);
            }
            if !self.ensure_client() {
                return None;
            }
            let client = self.client.as_ref()?;
            match client.bus_create(None) {
                Ok(resp) => {
                    self.bus_id = Some(resp.bus_id);
                    Some(resp.bus_id)
                }
                Err(e) => {
                    tracing::warn!("VIIPER bus_create failed: {e}");
                    None
                }
            }
        }

        fn warn_missing(&mut self, detail: &str) {
            if self.warned_missing {
                return;
            }
            self.warned_missing = true;
            tracing::warn!(
                "{detail}. Phone gamepad input is ignored until VIIPER + usbip-win2 are \
                 installed (bundled in the Windows installer)."
            );
        }

        fn plug(&mut self, pad: u8, connected: bool) {
            let idx = pad as usize;
            if idx >= 4 {
                return;
            }
            if connected {
                if self.pads[idx].is_some() {
                    return;
                }
                let Some(bus_id) = self.ensure_bus() else {
                    return;
                };
                let Some(client) = self.client.as_ref() else {
                    return;
                };
                let req = DeviceCreateRequest {
                    r#type: Some("xbox360".into()),
                    id_vendor: None,
                    id_product: None,
                    device_specific: None,
                };
                let device: Device = match client.bus_device_add(bus_id, &req) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("VIIPER add xbox360 pad {pad} failed: {e}");
                        // VIIPER removes empty buses; also recover after a server restart.
                        self.bus_id = None;
                        return;
                    }
                };
                let stream = match client.connect_device(bus_id, &device.dev_id) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("VIIPER connect pad {pad} failed: {e}");
                        let _ = client.bus_device_remove(bus_id, Some(&device.dev_id));
                        return;
                    }
                };
                tracing::info!(
                    "VIIPER virtual Xbox 360 pad {pad} connected ({})",
                    device.dev_id
                );
                self.pads[idx] = Some(PadSlot {
                    bus_id,
                    dev_id: device.dev_id,
                    stream: Some(stream),
                });
            } else if let Some(slot) = self.pads[idx].take() {
                if let Some(client) = self.client.as_ref() {
                    let _ = client.bus_device_remove(slot.bus_id, Some(&slot.dev_id));
                }
                if self.pads.iter().all(Option::is_none) {
                    // The final removal can delete the bus on the server.
                    if let (Some(client), Some(bus_id)) = (self.client.as_ref(), self.bus_id.take())
                    {
                        let _ = client.bus_remove(Some(bus_id));
                    }
                }
                tracing::info!("VIIPER virtual Xbox 360 pad {pad} disconnected");
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
            let idx = pad as usize;
            if idx >= 4 {
                return;
            }
            if self.pads[idx].is_none() {
                self.plug(pad, true);
            }
            let Some(slot) = self.pads[idx].as_mut() else {
                return;
            };
            let Some(stream) = slot.stream.as_mut() else {
                return;
            };
            let input = Xbox360Input {
                buttons: u32::from(buttons),
                lt,
                rt,
                lx,
                ly,
                rx,
                ry,
                ..Default::default()
            };
            if let Err(e) = stream.send(&input) {
                tracing::warn!("VIIPER pad {pad} update failed: {e}");
            }
        }

        fn reset(&mut self) {
            for pad in 0..4u8 {
                self.plug(pad, false);
            }
            if let (Some(client), Some(bus_id)) = (self.client.as_ref(), self.bus_id) {
                let _ = client.bus_remove(Some(bus_id));
            }
            self.bus_id = None;
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
        mgr().lock().unwrap().reset();
    }
}

#[cfg(windows)]
pub use imp::{apply, reset};

#[cfg(not(windows))]
pub fn apply(_event: InputEvent) {}

#[cfg(not(windows))]
pub fn reset() {}
