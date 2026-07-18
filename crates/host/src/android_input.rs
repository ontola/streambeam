//! Android touch frames waiting for the AccessibilityService injector.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use streambeam_proto::input::{InputEvent, TouchContact};

const MAX_PENDING: usize = 64;
static FRAMES: LazyLock<Mutex<VecDeque<(Vec<TouchContact>, u16, u16)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static ENABLED: AtomicBool = AtomicBool::new(false);
static MOUSE: LazyLock<Mutex<MouseState>> = LazyLock::new(|| Mutex::new(MouseState::default()));

#[derive(Default)]
struct MouseState {
    x: u16,
    y: u16,
    ref_w: u16,
    ref_h: u16,
    primary_down: bool,
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        FRAMES.lock().unwrap().clear();
        *MOUSE.lock().unwrap() = MouseState::default();
    }
}

pub fn submit(event: InputEvent) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    match event {
        InputEvent::TouchFrame {
            contacts,
            ref_w,
            ref_h,
        } => enqueue(contacts, ref_w, ref_h),
        InputEvent::MouseAbs { x, y, ref_w, ref_h } => {
            let mut mouse = MOUSE.lock().unwrap();
            mouse.x = x;
            mouse.y = y;
            mouse.ref_w = ref_w.max(1);
            mouse.ref_h = ref_h.max(1);
            if mouse.primary_down {
                enqueue_mouse(&mouse);
            }
        }
        InputEvent::MouseMove { dx, dy } => {
            let mut mouse = MOUSE.lock().unwrap();
            if mouse.ref_w == 0 || mouse.ref_h == 0 {
                return;
            }
            mouse.x = (i32::from(mouse.x) + i32::from(dx)).clamp(0, i32::from(mouse.ref_w)) as u16;
            mouse.y = (i32::from(mouse.y) + i32::from(dy)).clamp(0, i32::from(mouse.ref_h)) as u16;
            if mouse.primary_down {
                enqueue_mouse(&mouse);
            }
        }
        InputEvent::MouseButton { button: 0, down } => {
            let mut mouse = MOUSE.lock().unwrap();
            if mouse.ref_w == 0 || mouse.ref_h == 0 {
                return;
            }
            mouse.primary_down = down;
            if down {
                enqueue_mouse(&mouse);
            } else {
                enqueue(Vec::new(), mouse.ref_w, mouse.ref_h);
            }
        }
        _ => {}
    }
}

fn enqueue_mouse(mouse: &MouseState) {
    enqueue(
        vec![TouchContact {
            id: 0,
            x: mouse.x,
            y: mouse.y,
            pressure: u16::MAX,
        }],
        mouse.ref_w,
        mouse.ref_h,
    );
}

fn enqueue(contacts: Vec<TouchContact>, ref_w: u16, ref_h: u16) {
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

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn packs_full_touch_state() {
        let _guard = TEST_LOCK.lock().unwrap();
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

    #[test]
    fn maps_primary_mouse_drag_to_single_touch() {
        let _guard = TEST_LOCK.lock().unwrap();
        set_enabled(true);
        submit(InputEvent::MouseAbs {
            x: 10,
            y: 20,
            ref_w: 100,
            ref_h: 200,
        });
        submit(InputEvent::MouseButton {
            button: 0,
            down: true,
        });
        submit(InputEvent::MouseAbs {
            x: 30,
            y: 40,
            ref_w: 100,
            ref_h: 200,
        });
        submit(InputEvent::MouseButton {
            button: 0,
            down: false,
        });

        assert_eq!(
            take_packed(),
            Some(vec![100, 200, 1, 0, 10, 20, i32::from(u16::MAX)])
        );
        assert_eq!(
            take_packed(),
            Some(vec![100, 200, 1, 0, 30, 40, i32::from(u16::MAX)])
        );
        assert_eq!(take_packed(), Some(vec![100, 200, 0]));
        set_enabled(false);
    }
}
