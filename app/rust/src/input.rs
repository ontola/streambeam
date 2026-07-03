//! Touch/keyboard events forwarded to the host while a stream session is active.

use std::sync::{Mutex, OnceLock};

use streambeam_proto::input::InputEvent;
use tokio::sync::mpsc;

static INPUT_TX: OnceLock<Mutex<Option<mpsc::UnboundedSender<InputEvent>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<mpsc::UnboundedSender<InputEvent>>> {
    INPUT_TX.get_or_init(|| Mutex::new(None))
}

pub fn set_sender(tx: Option<mpsc::UnboundedSender<InputEvent>>) {
    *slot().lock().unwrap() = tx;
}

pub fn send(event: InputEvent) {
    let guard = slot().lock().unwrap();
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(event);
    }
}
