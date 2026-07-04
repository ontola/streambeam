//! Video encoder abstraction — hwcodec / MF / NVENC impls plug in behind this (M2).

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder not ready")]
    NotReady,
    #[error("{0}")]
    Other(String),
}

/// Host-side video encoder (desktop/WGC texture in → Annex-B access units out).
pub trait VideoEncoder: Send {
    /// Produce the next access unit. `Ok(None)` means "no frame this tick, try
    /// again" (e.g. the screen hasn't changed) — it is not an error.
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError>;
    fn force_idr(&mut self);
    fn dimensions(&self) -> (u32, u32);

    /// Tell the encoder the client lost video frames `[first_frame,
    /// last_frame]` (StreamBeam wrapping frame_ids) and the next frame must
    /// not reference them. Real hardware encoders map these to their
    /// reference frames and invalidate just those (NVENC
    /// `NvEncInvalidateRefFrames`, AMF/QSV equivalents) — a cheap P-frame
    /// instead of a full keyframe. The default falls back to a full IDR,
    /// which always recovers (just not cheaply); encoders that can't do real
    /// invalidation should leave this as-is rather than reimplement it.
    fn invalidate_ref_frames(&mut self, _first_frame: u16, _last_frame: u16) {
        self.force_idr();
    }

    /// SPS/PPS (Annex-B) for the `CodecConfig` control message, if known.
    fn codec_config(&self) -> Option<Vec<u8>> {
        None
    }
}
