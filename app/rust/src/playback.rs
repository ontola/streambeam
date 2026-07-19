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
static REMOTE_STOP_REQUESTED: AtomicU32 = AtomicU32::new(0);
/// Pending `RequestPower` from the UI: 0 = none, 1 = sleep, 2 = hibernate.
/// Polled by the platform stream loops, which own the control send stream.
static POWER_REQUEST: AtomicU32 = AtomicU32::new(0);
static ACTIVE_CONNECTION: Mutex<Option<quinn::Connection>> = Mutex::new(None);

/// Keeps the currently visible playback connection addressable from the UI's
/// synchronous Stop action. Dropping an older session must not clear a newer
/// one that already replaced it, hence the stable-id comparison.
pub struct ActiveConnectionGuard {
    stable_id: usize,
}

impl ActiveConnectionGuard {
    pub fn new(conn: &quinn::Connection) -> Self {
        let mut active = ACTIVE_CONNECTION.lock().unwrap();
        if let Some(previous) = active.replace(conn.clone())
            && previous.stable_id() != conn.stable_id()
        {
            previous.close(0u32.into(), b"playback replaced");
        }
        Self {
            stable_id: conn.stable_id(),
        }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        let mut active = ACTIVE_CONNECTION.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|conn| conn.stable_id() == self.stable_id)
        {
            *active = None;
        }
    }
}

pub fn stop_active_connection() {
    if let Some(conn) = ACTIVE_CONNECTION.lock().unwrap().take() {
        conn.close(0u32.into(), b"viewer stopped");
    }
}

pub fn reset() {
    FRAMES_RENDERED.store(0, Ordering::Relaxed);
    BYTES_RECEIVED.store(0, Ordering::Relaxed);
    NETWORK_RTT_US.store(0, Ordering::Relaxed);
    REMOTE_STOP_REQUESTED.store(0, Ordering::Relaxed);
}

pub fn remote_stop_requested() -> bool {
    REMOTE_STOP_REQUESTED.load(Ordering::Relaxed) != 0
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

/// UI request to suspend the connected host (see `POWER_REQUEST`).
pub fn request_host_power(hibernate: bool) {
    POWER_REQUEST.store(if hibernate { 2 } else { 1 }, Ordering::Relaxed);
}

/// Taken by the platform stream loops each tick; `Some(hibernate)`.
pub fn take_power_request() -> Option<bool> {
    match POWER_REQUEST.swap(0, Ordering::Relaxed) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

pub async fn receive_control(mut recv: quinn::RecvStream, conn: quinn::Connection) {
    let mut buf = Vec::new();
    while let Ok(message) = streambeam_net::read_msg::<HostMsg>(&mut recv, &mut buf).await {
        match message {
            HostMsg::ClipboardText { text } => receive_remote_clipboard(text),
            HostMsg::WakeInfo { mac, broadcast } => {
                // Cache Wake-on-LAN facts on the saved host so the app can
                // wake this machine later while it's unreachable. The saved
                // host is written on every successful connect, so by the
                // time WakeInfo arrives (right after session accept) it is
                // this host.
                let path = crate::store::saved_host_path();
                if let Ok(Some(mut host)) = streambeam_core::load_saved_host(&path) {
                    if host.mac != Some(mac) || host.wol_broadcast != broadcast {
                        host.mac = Some(mac);
                        host.wol_broadcast = broadcast;
                        let _ = streambeam_core::save_saved_host(&path, &host);
                    }
                }
            }
            HostMsg::Bye => {
                // Wake the media read immediately. Its error completes the
                // Rust playback future/stream, and Flutter tears down the
                // frozen last frame in the same turn.
                REMOTE_STOP_REQUESTED.store(1, Ordering::Relaxed);
                conn.close(0u32.into(), b"host stopped stream");
                break;
            }
            _ => {}
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
