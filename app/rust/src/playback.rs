//! Live playback counters polled from Dart while a stream session is active.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
struct SessionInfo {
    width: u16,
    height: u16,
    fps: u8,
    codec: String,
}

static FRAMES_RENDERED: AtomicU64 = AtomicU64::new(0);
static BYTES_RECEIVED: AtomicU64 = AtomicU64::new(0);
static SESSION: Mutex<Option<SessionInfo>> = Mutex::new(None);

pub fn reset() {
    FRAMES_RENDERED.store(0, Ordering::Relaxed);
    BYTES_RECEIVED.store(0, Ordering::Relaxed);
}

pub fn set_session(width: u16, height: u16, fps: u8, codec: String) {
    *SESSION.lock().unwrap() = Some(SessionInfo {
        width,
        height,
        fps,
        codec,
    });
}

pub fn session_width() -> u16 {
    SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.width)
        .unwrap_or(0)
}

pub fn session_height() -> u16 {
    SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.height)
        .unwrap_or(0)
}

pub fn session_fps() -> u8 {
    SESSION.lock().unwrap().as_ref().map(|s| s.fps).unwrap_or(0)
}

pub fn session_codec() -> String {
    SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.codec.clone())
        .unwrap_or_default()
}

pub fn note_frame_rendered() {
    FRAMES_RENDERED.fetch_add(1, Ordering::Relaxed);
}

pub fn note_bytes_received(n: u64) {
    BYTES_RECEIVED.fetch_add(n, Ordering::Relaxed);
}

pub fn frames_rendered() -> u64 {
    FRAMES_RENDERED.load(Ordering::Relaxed)
}

pub fn bytes_received() -> u64 {
    BYTES_RECEIVED.load(Ordering::Relaxed)
}
