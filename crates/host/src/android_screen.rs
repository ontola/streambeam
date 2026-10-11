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
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder};

/// Bound latency without ever forwarding dependent frames after a local drop.
const MAX_PENDING: usize = 8;

struct FrameQueue {
    frames: VecDeque<EncodedFrame>,
    waiting_for_keyframe: bool,
}

impl FrameQueue {
    const fn new() -> Self {
        Self {
            frames: VecDeque::new(),
            waiting_for_keyframe: true,
        }
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.waiting_for_keyframe = true;
    }

    /// True requests one fresh keyframe after overflow. Wire sequence IDs are
    /// assigned later, so the receiver cannot detect these sender-side drops.
    fn push(&mut self, frame: EncodedFrame) -> bool {
        let overflow = self.frames.len() >= MAX_PENDING;
        if overflow {
            self.clear();
        }
        if frame.keyframe {
            self.waiting_for_keyframe = false;
        } else if self.waiting_for_keyframe {
            return overflow;
        }
        self.frames.push_back(frame);
        false
    }

    fn pop_front(&mut self) -> Option<EncodedFrame> {
        self.frames.pop_front()
    }
}

struct ScreenSource {
    pending: Mutex<FrameQueue>,
    codec_config: Mutex<Option<Vec<u8>>>,
    idr_requested: AtomicBool,
    active: AtomicBool,
    capture_wanted: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
}

static SOURCE: ScreenSource = ScreenSource {
    pending: Mutex::new(FrameQueue::new()),
    codec_config: Mutex::new(None),
    idr_requested: AtomicBool::new(false),
    active: AtomicBool::new(false),
    capture_wanted: AtomicBool::new(false),
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

/// Whether the codec config (SPS/PPS) has arrived — i.e. the session is fully
/// ready to encode. `build_encoder` waits for this so it doesn't fall back to
/// the mock clip during the async capture-service startup.
pub fn is_ready() -> bool {
    is_active() && SOURCE.codec_config.lock().unwrap().is_some()
}

/// Mark that an authenticated client is waiting for this screen. The Android
/// activity polls this flag and opens the mandatory MediaProjection consent
/// UI; no capture prompt is raised merely because the idle host is listening.
pub fn request_capture() {
    SOURCE.capture_wanted.store(true, Ordering::Release);
}

/// Release the current client's capture lease. The Android activity observes
/// this and stops the foreground capture service while leaving the QUIC host
/// online for the next paired client.
pub fn release_capture() {
    SOURCE.capture_wanted.store(false, Ordering::Release);
}

pub fn capture_wanted() -> bool {
    SOURCE.capture_wanted.load(Ordering::Acquire)
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
    let needs_idr = q.push(EncodedFrame {
        data,
        keyframe,
        width: w,
        height: h,
        timestamp: 0,
    });
    if needs_idr {
        SOURCE.idr_requested.store(true, Ordering::Relaxed);
        crate::logging::log("Android capture queue overflow: waiting for a fresh keyframe");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u64, keyframe: bool) -> EncodedFrame {
        EncodedFrame {
            data: vec![id as u8],
            keyframe,
            width: 2560,
            height: 1600,
            timestamp: id,
        }
    }

    #[test]
    fn overflow_never_forwards_a_broken_reference_chain() {
        let mut q = FrameQueue::new();
        for id in 0..MAX_PENDING as u64 {
            assert!(!q.push(frame(id, id == 0)));
        }
        assert!(q.push(frame(8, false)), "overflow must request recovery");
        assert!(q.pop_front().is_none());
        for id in 9..20 {
            assert!(!q.push(frame(id, false)), "request only once per overflow");
            assert!(
                q.pop_front().is_none(),
                "dependent frames must be discarded"
            );
        }
        assert!(!q.push(frame(20, true)));
        assert!(!q.push(frame(21, false)));
        assert_eq!(q.pop_front().unwrap().timestamp, 20);
        assert_eq!(q.pop_front().unwrap().timestamp, 21);
    }

    #[test]
    fn overflowing_keyframe_recovers_without_an_extra_request() {
        let mut q = FrameQueue::new();
        for id in 0..MAX_PENDING as u64 {
            q.push(frame(id, id == 0));
        }
        assert!(!q.push(frame(8, true)));
        assert_eq!(q.pop_front().unwrap().timestamp, 8);
        assert!(q.pop_front().is_none());
    }

    #[test]
    fn healthy_frames_stay_ordered_and_a_new_capture_waits_for_keyframe() {
        let mut q = FrameQueue::new();
        q.push(frame(0, false));
        assert!(q.pop_front().is_none());
        for id in 1..100 {
            assert!(!q.push(frame(id, id == 1)));
            assert_eq!(q.pop_front().unwrap().timestamp, id);
        }
        q.clear();
        q.push(frame(100, false));
        assert!(q.pop_front().is_none());
        q.push(frame(101, true));
        assert_eq!(q.pop_front().unwrap().timestamp, 101);
    }
}
