//! Live playback counters polled from Dart while a stream session is active.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES_RENDERED: AtomicU64 = AtomicU64::new(0);

pub fn reset() {
    FRAMES_RENDERED.store(0, Ordering::Relaxed);
}

pub fn note_frame_rendered() {
    FRAMES_RENDERED.fetch_add(1, Ordering::Relaxed);
}

pub fn frames_rendered() -> u64 {
    FRAMES_RENDERED.load(Ordering::Relaxed)
}
