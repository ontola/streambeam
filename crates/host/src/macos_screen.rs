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

const MAX_PENDING: usize = 8;
/// Must match `StreamBeamCapture.fps` in the macOS runner. Kept explicit until
/// capture settings are passed across the native bridge dynamically.
pub const CAPTURE_FPS: u32 = 30;

struct ScreenSource {
    pending: Mutex<VecDeque<EncodedFrame>>,
    codec_config: Mutex<Option<Vec<u8>>>,
    idr_requested: AtomicBool,
    active: AtomicBool,
    /// Set while an authorized client owns the active stream. The Swift
    /// runner polls this and starts/stops ScreenCaptureKit on demand.
    capture_wanted: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
}

static SOURCE: ScreenSource = ScreenSource {
    pending: Mutex::new(VecDeque::new()),
    codec_config: Mutex::new(None),
    idr_requested: AtomicBool::new(false),
    active: AtomicBool::new(false),
    capture_wanted: AtomicBool::new(false),
    width: AtomicU32::new(0),
    height: AtomicU32::new(0),
};

pub fn begin_session(width: u32, height: u32) {
    SOURCE.width.store(width, Ordering::Relaxed);
    SOURCE.height.store(height, Ordering::Relaxed);
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
    SOURCE.idr_requested.store(false, Ordering::Relaxed);
    SOURCE.active.store(true, Ordering::Relaxed);
}

pub fn end_session() {
    SOURCE.active.store(false, Ordering::Relaxed);
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
}

pub fn is_ready() -> bool {
    SOURCE.active.load(Ordering::Relaxed) && SOURCE.codec_config.lock().unwrap().is_some()
}

pub fn request_capture() {
    SOURCE.capture_wanted.store(true, Ordering::Release);
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
        request_capture();
        assert!(capture_wanted());
        begin_session(1920, 1080);
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
