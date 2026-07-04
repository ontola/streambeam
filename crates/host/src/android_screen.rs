//! Android screen-capture video source.
//!
//! On Android the host server has no desktop-duplication path; instead the
//! Kotlin side runs `MediaProjection` + a `MediaCodec` H.264 encoder (Surface
//! input, `VirtualDisplay` output) and pushes the encoded Annex-B access units
//! into the [`SOURCE`] here via JNI. The host server pulls them back out
//! through the [`AndroidScreenEncoder`] `VideoEncoder` impl — so the entire
//! QUIC / pairing / FEC send loop is reused unchanged; only the frame source
//! differs from the Windows desktop path.
//!
//! Sequencing (owned by the app FFI / Kotlin): [`begin_session`] with the
//! capture dimensions → submit the codec-config (SPS/PPS) CSD buffer via
//! [`submit`] with `is_config = true` → start the host server (which reads
//! [`AndroidScreenEncoder::codec_config`] once at startup) → stream frames.
//! [`take_idr_request`] is polled by Kotlin to know when to ask MediaCodec for
//! a sync frame in response to a client's `RequestIdr`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder};

/// Max buffered access units before the oldest is dropped. The screen stream is
/// live, so a backlog means the link can't keep up — better to drop and let the
/// client recover via RFI/IDR than to let latency grow unbounded.
const MAX_PENDING: usize = 8;

struct ScreenSource {
    pending: Mutex<VecDeque<EncodedFrame>>,
    codec_config: Mutex<Option<Vec<u8>>>,
    idr_requested: AtomicBool,
    active: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
}

static SOURCE: ScreenSource = ScreenSource {
    pending: Mutex::new(VecDeque::new()),
    codec_config: Mutex::new(None),
    idr_requested: AtomicBool::new(false),
    active: AtomicBool::new(false),
    width: AtomicU32::new(0),
    height: AtomicU32::new(0),
};

/// Begin a capture session with the negotiated frame size. Clears any state
/// from a previous session. Call before starting the host server.
pub fn begin_session(width: u32, height: u32) {
    SOURCE.width.store(width, Ordering::Relaxed);
    SOURCE.height.store(height, Ordering::Relaxed);
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
    SOURCE.idr_requested.store(false, Ordering::Relaxed);
    SOURCE.active.store(true, Ordering::Relaxed);
}

/// Tear down the capture session and drop any buffered frames.
pub fn end_session() {
    SOURCE.active.store(false, Ordering::Relaxed);
    SOURCE.pending.lock().unwrap().clear();
    *SOURCE.codec_config.lock().unwrap() = None;
}

/// Whether a capture session is currently active (used by `build_encoder`).
pub fn is_active() -> bool {
    SOURCE.active.load(Ordering::Relaxed)
}

/// Submit one `MediaCodec` output buffer. `is_config` marks the CSD (SPS/PPS)
/// buffer, which is stored for `codec_config()` rather than queued as a frame.
pub fn submit(data: Vec<u8>, keyframe: bool, is_config: bool, width: u32, height: u32) {
    if width > 0 {
        SOURCE.width.store(width, Ordering::Relaxed);
    }
    if height > 0 {
        SOURCE.height.store(height, Ordering::Relaxed);
    }
    if is_config {
        *SOURCE.codec_config.lock().unwrap() = Some(data);
        return;
    }
    let (w, h) = (
        SOURCE.width.load(Ordering::Relaxed),
        SOURCE.height.load(Ordering::Relaxed),
    );
    let mut q = SOURCE.pending.lock().unwrap();
    while q.len() >= MAX_PENDING {
        q.pop_front();
    }
    q.push_back(EncodedFrame {
        data,
        keyframe,
        width: w,
        height: h,
    });
}

/// Consume a pending "request a keyframe" flag. Kotlin polls this and, when
/// true, asks MediaCodec for a sync frame (`PARAMETER_KEY_REQUEST_SYNC_FRAME`).
pub fn take_idr_request() -> bool {
    SOURCE.idr_requested.swap(false, Ordering::Relaxed)
}

/// `VideoEncoder` that pulls live screen frames from [`SOURCE`].
pub struct AndroidScreenEncoder;

impl VideoEncoder for AndroidScreenEncoder {
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        // `None` means "no frame this tick" — the server paces and retries,
        // same contract as the mock encoder.
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
