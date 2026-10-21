//! Restricted browser commands. No clipboard, power actions, SAS, or arbitrary proto JSON.
use super::Error;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use streambeam_proto::input::InputEvent;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Command {
    Control { enabled: bool },
    Move { x: u16, y: u16 },
    Button { button: u8, down: bool },
    Key { vk: u16, down: bool },
    Scroll { dx: i16, dy: i16 },
}

pub struct Control {
    allowed: bool,
    enabled: bool,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    window: Instant,
    count: u16,
}
impl Control {
    pub fn new(allowed: bool) -> Self {
        Self {
            allowed,
            enabled: false,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            window: Instant::now(),
            count: 0,
        }
    }
    pub fn release(&mut self) -> Vec<InputEvent> {
        self.enabled = false;
        std::mem::take(&mut self.keys)
            .into_iter()
            .map(|vk| InputEvent::Key { vk, down: false })
            .chain(std::mem::take(&mut self.buttons).into_iter().map(|button| {
                InputEvent::MouseButton {
                    button,
                    down: false,
                }
            }))
            .collect()
    }
    pub fn accept(
        &mut self,
        command: Command,
        width: u16,
        height: u16,
    ) -> Result<Vec<InputEvent>, Error> {
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = Instant::now();
            self.count = 0;
        }
        self.count += 1;
        if self.count > 250 {
            return Err("browser input rate exceeded".into());
        }
        if let Command::Control { enabled } = command {
            if !enabled {
                return Ok(self.release());
            }
            self.enabled = self.allowed;
            return Ok(Vec::new());
        }
        if !self.enabled {
            return Ok(Vec::new());
        }
        let event = match command {
            Command::Move { x, y } if x < width && y < height => InputEvent::MouseAbs {
                x,
                y,
                ref_w: width,
                ref_h: height,
            },
            Command::Button { button, down } if button <= 2 => {
                if down {
                    self.buttons.insert(button);
                } else if !self.buttons.remove(&button) {
                    return Ok(Vec::new());
                }
                InputEvent::MouseButton { button, down }
            }
            Command::Key { vk, down } if (8..=255).contains(&vk) => {
                if down {
                    self.keys.insert(vk);
                } else if !self.keys.remove(&vk) {
                    return Ok(Vec::new());
                }
                if self.keys.len() > 16 {
                    return Err("too many held browser keys".into());
                }
                InputEvent::Key { vk, down }
            }
            Command::Scroll { dx, dy } if dx.abs_diff(0) <= 1200 && dy.abs_diff(0) <= 1200 => {
                InputEvent::Scroll { dx, dy }
            }
            _ => return Err("invalid browser input".into()),
        };
        Ok(vec![event])
    }
}

pub async fn read(
    mut recv: wtransport::RecvStream,
    tx: tokio::sync::mpsc::Sender<Result<Command, Error>>,
) {
    loop {
        let message = async {
            let mut prefix = [0; 2];
            recv.read_exact(&mut prefix).await?;
            let size = u16::from_be_bytes(prefix) as usize;
            if size == 0 || size > 512 {
                return Err::<_, Error>("invalid input packet size".into());
            }
            let mut data = vec![0; size];
            tokio::time::timeout(Duration::from_secs(5), recv.read_exact(&mut data)).await??;
            Ok(serde_json::from_slice::<Command>(&data)?)
        }
        .await;
        let failed = message.is_err();
        if tx.send(message).await.is_err() || failed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_explicit_consent_and_releases_on_disable() {
        let mut c = Control::new(true);
        assert!(
            c.accept(Command::Key { vk: 65, down: true }, 640, 360)
                .unwrap()
                .is_empty()
        );
        c.accept(Command::Control { enabled: true }, 640, 360)
            .unwrap();
        c.accept(Command::Key { vk: 65, down: true }, 640, 360)
            .unwrap();
        c.accept(Command::Key { vk: 65, down: true }, 640, 360)
            .unwrap();
        c.accept(
            Command::Button {
                button: 0,
                down: true,
            },
            640,
            360,
        )
        .unwrap();
        assert_eq!(
            c.accept(Command::Control { enabled: false }, 640, 360)
                .unwrap(),
            vec![
                InputEvent::Key {
                    vk: 65,
                    down: false
                },
                InputEvent::MouseButton {
                    button: 0,
                    down: false
                }
            ]
        );
        assert!(c.release().is_empty());
        assert!(
            c.accept(Command::Key { vk: 66, down: true }, 640, 360)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn rejects_privileged_unknown_and_out_of_range_input() {
        assert!(serde_json::from_str::<Command>(r#"{"type":"power","action":"Sleep"}"#).is_err());
        assert!(
            serde_json::from_str::<Command>(r#"{"type":"key","vk":65,"down":true,"extra":1}"#)
                .is_err()
        );
        let mut c = Control::new(true);
        c.accept(Command::Control { enabled: true }, 640, 360)
            .unwrap();
        assert!(c.accept(Command::Move { x: 640, y: 2 }, 640, 360).is_err());
        assert!(
            c.accept(
                Command::Button {
                    button: 8,
                    down: true
                },
                640,
                360
            )
            .is_err()
        );
        assert!(
            c.accept(
                Command::Key {
                    vk: 300,
                    down: true
                },
                640,
                360
            )
            .is_err()
        );
        assert!(
            c.accept(
                Command::Scroll {
                    dx: i16::MIN,
                    dy: 0
                },
                640,
                360
            )
            .is_err()
        );
    }
    #[test]
    fn touch_hosts_cannot_enable_desktop_control() {
        let mut c = Control::new(false);
        c.accept(Command::Control { enabled: true }, 640, 360)
            .unwrap();
        assert!(
            c.accept(Command::Key { vk: 65, down: true }, 640, 360)
                .unwrap()
                .is_empty()
        );
    }
}
