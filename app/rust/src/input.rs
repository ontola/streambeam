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

/// Drain the input channel onto the session's input stream (S1), coalescing
/// bursts of relative mouse moves. Shared by the Android and desktop watch
/// paths — spawn it once per session and abort it when the session ends.
pub async fn pump(
    mut send: streambeam_net::quinn::SendStream,
    mut rx: mpsc::UnboundedReceiver<InputEvent>,
) {
    while let Some(first) = rx.recv().await {
        for ev in coalesce_batch(first, &mut rx) {
            if streambeam_net::write_msg(&mut send, &ev).await.is_err() {
                return;
            }
        }
    }
}

fn coalesce_batch(
    first: InputEvent,
    rx: &mut mpsc::UnboundedReceiver<InputEvent>,
) -> Vec<InputEvent> {
    let mut batch = vec![first];
    while let Ok(next) = rx.try_recv() {
        batch.push(next);
        if batch.len() >= 64 {
            break;
        }
    }

    let mut out = Vec::with_capacity(batch.len());
    let mut i = 0;
    while i < batch.len() {
        match batch[i] {
            InputEvent::MouseMove { dx, dy } => {
                let mut sum_dx = i32::from(dx);
                let mut sum_dy = i32::from(dy);
                let mut j = i + 1;
                while j < batch.len() {
                    match batch[j] {
                        InputEvent::MouseMove { dx, dy } => {
                            sum_dx = sum_dx.saturating_add(i32::from(dx));
                            sum_dy = sum_dy.saturating_add(i32::from(dy));
                            j += 1;
                        }
                        _ => break,
                    }
                }
                out.push(InputEvent::MouseMove {
                    dx: sum_dx.clamp(-32768, 32767) as i16,
                    dy: sum_dy.clamp(-32768, 32767) as i16,
                });
                i = j;
            }
            ref other => {
                out.push(other.clone());
                i += 1;
            }
        }
    }
    out
}
