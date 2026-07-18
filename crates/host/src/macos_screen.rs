//! macOS ScreenCaptureKit + VideoToolbox frame source.
//!
//! The Flutter runner owns the native Apple APIs and pushes H.264 Annex-B
//! access units into this bounded queue. The platform-independent host server
//! pulls them through [`MacOsScreenEncoder`], reusing pairing, QUIC, FEC and
//! recovery unchanged.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder};
use streambeam_proto::control::DisplayInfo;

const MAX_PENDING: usize = 8;
/// Must match `StreamBeamCapture.fps` in the macOS runner. Kept explicit until
/// capture settings are passed across the native bridge dynamically.
pub const CAPTURE_FPS: u32 = 30;
const NO_DISPLAY: u32 = u32::MAX;

struct ScreenSource {
    pending: Mutex<VecDeque<EncodedFrame>>,
    codec_config: Mutex<Option<Vec<u8>>>,
    idr_requested: AtomicBool,
    active: AtomicBool,
    /// Set while an authorized client owns the active stream. The Swift
    /// runner polls this and starts/stops ScreenCaptureKit on demand.
    capture_wanted: AtomicBool,
    /// Longest encoded side requested by the client that owns the stream.
    /// Swift reads this immediately before starting ScreenCaptureKit.
    requested_max_dimension: AtomicU32,
    /// Monotonically changes for every client-owned capture session. Native
    /// code uses it to notice a stop/start handoff even when its polling
    /// interval misses the brief `capture_wanted = false` transition.
    capture_request_revision: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
    requested_display: AtomicU32,
    selected_display: AtomicU32,
    displays: Mutex<Vec<DisplayInfo>>,
    display_bounds: Mutex<(f64, f64, f64, f64)>,
}

static SOURCE: ScreenSource = ScreenSource {
    pending: Mutex::new(VecDeque::new()),
    codec_config: Mutex::new(None),
    idr_requested: AtomicBool::new(false),
    active: AtomicBool::new(false),
    capture_wanted: AtomicBool::new(false),
    requested_max_dimension: AtomicU32::new(1920),
    capture_request_revision: AtomicU32::new(0),
    width: AtomicU32::new(0),
    height: AtomicU32::new(0),
    requested_display: AtomicU32::new(NO_DISPLAY),
    selected_display: AtomicU32::new(0),
    displays: Mutex::new(Vec::new()),
    display_bounds: Mutex::new((0.0, 0.0, 1.0, 1.0)),
};

pub fn begin_session(
    width: u32,
    height: u32,
    display_id: u32,
    origin_x: f64,
    origin_y: f64,
    display_width: f64,
    display_height: f64,
) {
    SOURCE.width.store(width, Ordering::Relaxed);
    SOURCE.height.store(height, Ordering::Relaxed);
    SOURCE.selected_display.store(display_id, Ordering::Relaxed);
    *SOURCE.display_bounds.lock().unwrap() = (
        origin_x,
        origin_y,
        display_width.max(1.0),
        display_height.max(1.0),
    );
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
    SOURCE.idr_requested.store(false, Ordering::Relaxed);
    SOURCE.active.store(true, Ordering::Relaxed);
}

pub fn set_requested_display(display_id: Option<u32>) {
    SOURCE
        .requested_display
        .store(display_id.unwrap_or(NO_DISPLAY), Ordering::Release);
}

pub fn requested_display() -> Option<u32> {
    match SOURCE.requested_display.load(Ordering::Acquire) {
        NO_DISPLAY => None,
        id => Some(id),
    }
}

pub fn set_displays(displays: Vec<DisplayInfo>) {
    *SOURCE.displays.lock().unwrap() = displays;
}

pub fn displays() -> Vec<DisplayInfo> {
    SOURCE.displays.lock().unwrap().clone()
}

pub fn selected_display() -> u32 {
    SOURCE.selected_display.load(Ordering::Relaxed)
}

pub fn selected_display_bounds() -> (f64, f64, f64, f64) {
    *SOURCE.display_bounds.lock().unwrap()
}

pub fn end_session() {
    SOURCE.active.store(false, Ordering::Relaxed);
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
}

pub fn is_ready() -> bool {
    SOURCE.active.load(Ordering::Relaxed) && SOURCE.codec_config.lock().unwrap().is_some()
}

pub fn request_capture(max_dimension: u32) {
    // A cached native source must not make the next session's build return
    // early with stale dimensions/display. Swift will rebuild for the new
    // revision and call `begin_session` once its codec config is ready.
    end_session();
    SOURCE
        .requested_max_dimension
        .store(max_dimension.max(2), Ordering::Release);
    SOURCE
        .capture_request_revision
        .fetch_add(1, Ordering::AcqRel);
    SOURCE.capture_wanted.store(true, Ordering::Release);
}

pub fn requested_max_dimension() -> u32 {
    SOURCE
        .requested_max_dimension
        .load(Ordering::Acquire)
        .max(2)
}

pub fn capture_request_revision() -> u32 {
    SOURCE.capture_request_revision.load(Ordering::Acquire)
}

pub fn release_capture() {
    SOURCE.capture_wanted.store(false, Ordering::Release);
}

pub fn capture_wanted() -> bool {
    SOURCE.capture_wanted.load(Ordering::Acquire)
}

pub fn submit(data: Vec<u8>, keyframe: bool, is_config: bool) {
    if !SOURCE.active.load(Ordering::Relaxed) {
        return;
    }
    if is_config {
        *SOURCE.codec_config.lock().unwrap() = Some(data);
        return;
    }
    let mut pending = SOURCE.pending.lock().unwrap();
    while pending.len() >= MAX_PENDING {
        pending.pop_front();
    }
    pending.push_back(EncodedFrame {
        data,
        keyframe,
        width: SOURCE.width.load(Ordering::Relaxed).max(1),
        height: SOURCE.height.load(Ordering::Relaxed).max(1),
        timestamp: 0,
    });
}

pub fn take_idr_request() -> bool {
    SOURCE.idr_requested.swap(false, Ordering::Relaxed)
}

pub struct MacOsScreenEncoder;

impl VideoEncoder for MacOsScreenEncoder {
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        Ok(SOURCE.pending.lock().unwrap().pop_front())
    }

    fn force_idr(&mut self) {
        SOURCE.idr_requested.store(true, Ordering::Relaxed);
    }

    fn dimensions(&self) -> (u32, u32) {
        (
            SOURCE.width.load(Ordering::Relaxed).max(1),
            SOURCE.height.load(Ordering::Relaxed).max(1),
        )
    }

    fn codec_config(&self) -> Option<Vec<u8>> {
        SOURCE.codec_config.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_frames_flow_through_bounded_encoder_queue() {
        end_session();
        release_capture();
        request_capture(1280);
        assert!(capture_wanted());
        assert_eq!(requested_max_dimension(), 1280);
        begin_session(1920, 1080, 1, 0.0, 0.0, 1920.0, 1080.0);
        assert!(!is_ready());
        submit(vec![0, 0, 0, 1, 0x67], true, true);
        assert!(is_ready());

        for frame in 0..(MAX_PENDING + 2) {
            submit(vec![frame as u8], frame == 0, false);
        }

        let mut encoder = MacOsScreenEncoder;
        assert_eq!(encoder.dimensions(), (1920, 1080));
        assert_eq!(encoder.codec_config(), Some(vec![0, 0, 0, 1, 0x67]));
        // The two oldest live frames were dropped once the bounded queue filled.
        assert_eq!(encoder.encode_next().unwrap().unwrap().data, vec![2]);
        encoder.force_idr();
        assert!(take_idr_request());
        assert!(!take_idr_request());
        end_session();
        release_capture();
        assert!(!capture_wanted());
        assert!(encoder.encode_next().unwrap().is_none());
    }
}
