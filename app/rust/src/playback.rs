//! Live playback counters polled from Dart while a stream session is active.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use streambeam_proto::control::DisplayInfo;
use streambeam_proto::control::HostMsg;

#[derive(Debug, Clone)]
struct SessionInfo {
    width: u16,
    height: u16,
    fps: u8,
    codec: String,
}

static FRAMES_RENDERED: AtomicU64 = AtomicU64::new(0);
static BYTES_RECEIVED: AtomicU64 = AtomicU64::new(0);
static NETWORK_RTT_US: AtomicU64 = AtomicU64::new(0);
static SESSION: Mutex<Option<SessionInfo>> = Mutex::new(None);
static DISPLAYS: Mutex<Vec<DisplayInfo>> = Mutex::new(Vec::new());
static REMOTE_CLIPBOARD: Mutex<Option<String>> = Mutex::new(None);
const NO_DISPLAY: u32 = u32::MAX;
static REQUESTED_DISPLAY: AtomicU32 = AtomicU32::new(NO_DISPLAY);
static HOST_ACCEPTS_TOUCH: AtomicU32 = AtomicU32::new(0);

pub fn reset() {
    FRAMES_RENDERED.store(0, Ordering::Relaxed);
    BYTES_RECEIVED.store(0, Ordering::Relaxed);
    NETWORK_RTT_US.store(0, Ordering::Relaxed);
}

pub fn set_session(width: u16, height: u16, fps: u8, codec: String) {
    *SESSION.lock().unwrap() = Some(SessionInfo {
        width,
        height,
        fps,
        codec,
    });
}

pub fn update_dimensions(width: u16, height: u16) {
    if width == 0 || height == 0 {
        return;
    }
    if let Some(session) = SESSION.lock().unwrap().as_mut() {
        session.width = width;
        session.height = height;
    }
}

pub fn set_displays(displays: Vec<DisplayInfo>, selected_id: u32) {
    *DISPLAYS.lock().unwrap() = displays;
    if REQUESTED_DISPLAY.load(Ordering::Relaxed) == NO_DISPLAY {
        REQUESTED_DISPLAY.store(selected_id, Ordering::Relaxed);
    }
}

pub fn displays() -> Vec<DisplayInfo> {
    DISPLAYS.lock().unwrap().clone()
}

pub fn requested_display_id() -> Option<u32> {
    match REQUESTED_DISPLAY.load(Ordering::Relaxed) {
        NO_DISPLAY => None,
        id => Some(id),
    }
}

pub fn request_display(id: u32) {
    REQUESTED_DISPLAY.store(id, Ordering::Relaxed);
}

pub fn set_host_accepts_touch(accepts: bool) {
    HOST_ACCEPTS_TOUCH.store(u32::from(accepts), Ordering::Relaxed);
}

pub fn host_accepts_touch() -> bool {
    HOST_ACCEPTS_TOUCH.load(Ordering::Relaxed) != 0
}

pub fn receive_remote_clipboard(text: String) {
    if text.len() <= 60_000 {
        *REMOTE_CLIPBOARD.lock().unwrap() = Some(text);
    }
}

pub fn take_remote_clipboard() -> Option<String> {
    REMOTE_CLIPBOARD.lock().unwrap().take()
}

pub async fn receive_control(mut recv: quinn::RecvStream) {
    let mut buf = Vec::new();
    while let Ok(message) = streambeam_net::read_msg::<HostMsg>(&mut recv, &mut buf).await {
        if let HostMsg::ClipboardText { text } = message {
            receive_remote_clipboard(text);
        }
    }
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

pub fn note_network_rtt(rtt: std::time::Duration) {
    NETWORK_RTT_US.store(
        rtt.as_micros().min(u128::from(u64::MAX)) as u64,
        Ordering::Relaxed,
    );
}

pub fn frames_rendered() -> u64 {
    FRAMES_RENDERED.load(Ordering::Relaxed)
}

pub fn bytes_received() -> u64 {
    BYTES_RECEIVED.load(Ordering::Relaxed)
}

pub fn network_rtt_us() -> u64 {
    NETWORK_RTT_US.load(Ordering::Relaxed)
}
