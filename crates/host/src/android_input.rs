//! Android touch frames waiting for the AccessibilityService injector.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use streambeam_proto::input::{InputEvent, TouchContact};

const MAX_PENDING: usize = 64;
static FRAMES: LazyLock<Mutex<VecDeque<(Vec<TouchContact>, u16, u16)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        FRAMES.lock().unwrap().clear();
    }
}

pub fn submit(event: InputEvent) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let InputEvent::TouchFrame {
        contacts,
        ref_w,
        ref_h,
    } = event
    else {
        return;
    };
    let mut frames = FRAMES.lock().unwrap();
    if frames.len() >= MAX_PENDING {
        // Preserve state transitions at both ends. Dropping intermediate move
        // frames is safe because every frame contains the full active set.
        if frames.len() > 1 {
            frames.remove(1);
        } else {
            frames.pop_front();
        }
    }
    frames.push_back((contacts, ref_w.max(1), ref_h.max(1)));
}

/// Packed as `[ref_w, ref_h, count, id, x, y, pressure, ...]` for JNI.
pub fn take_packed() -> Option<Vec<i32>> {
    let (contacts, ref_w, ref_h) = FRAMES.lock().unwrap().pop_front()?;
    let mut packed = Vec::with_capacity(3 + contacts.len() * 4);
    packed.extend([i32::from(ref_w), i32::from(ref_h), contacts.len() as i32]);
    for contact in contacts {
        packed.extend([
            i32::from(contact.id),
            i32::from(contact.x),
            i32::from(contact.y),
            i32::from(contact.pressure),
        ]);
    }
    Some(packed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_full_touch_state() {
        set_enabled(true);
        submit(InputEvent::TouchFrame {
            contacts: vec![TouchContact {
                id: 7,
                x: 11,
                y: 13,
                pressure: 17,
            }],
            ref_w: 100,
            ref_h: 200,
        });
        assert_eq!(take_packed(), Some(vec![100, 200, 1, 7, 11, 13, 17]));
        set_enabled(false);
    }
}
