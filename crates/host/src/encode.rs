//! Video encoder abstraction — hwcodec / MF / NVENC impls plug in behind this (M2).

use thiserror::Error;

/// The codec a client asked for (`VideoCaps.av1`) and/or an encoder actually
/// produced. Lives here rather than in the Windows-only `nvenc` module so
/// `server.rs` (cross-platform) can reason about it without pulling in any
/// platform-specific encoder code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    H264,
    Av1,
}

impl Codec {
    pub fn wire_name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Av1 => "av01",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    /// Encoder-internal timestamp of this frame (NVENC: the
    /// `inputTimeStamp`/`outputTimeStamp` the session tracks its DPB by),
    /// used to name reference frames in `invalidate_ref_timestamps`.
    /// Encoders without reference invalidation just report 0 — the value is
    /// only ever handed back to the same encoder that produced it.
    pub timestamp: u64,
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder not ready")]
    NotReady,
    #[error("{0}")]
    Other(String),
}

/// Starting bitrate before any adaptive adjustment (`server.rs`'s controller
/// pulls this down fast on real loss/congestion and eases it back up on
/// sustained clean windows — see `stream_video`). Ceiling raised from an
/// earlier flat 20 Mbps cap, which was low enough to visibly soften detail
/// on a LAN connection that can trivially sustain far more (RTT ~5ms, zero
/// loss in practice); the ceiling only matters when the network genuinely
/// has that headroom.
pub fn default_bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
    ((width * height) as u64 * fps as u64 / 15).clamp(2_000_000, 60_000_000) as u32
}

/// Host-side video encoder (desktop/WGC texture in → Annex-B access units out).
pub trait VideoEncoder: Send {
    /// Produce the next access unit. `Ok(None)` means "no frame this tick, try
    /// again" (e.g. the screen hasn't changed) — it is not an error.
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError>;
    fn force_idr(&mut self);
    fn dimensions(&self) -> (u32, u32);

    /// Tell the encoder the client lost the frames with these
    /// encoder-internal timestamps (`EncodedFrame::timestamp` values —
    /// `server.rs` keeps the wire-frame-id → timestamp mapping and resolves
    /// the client's `InvalidateRefFrames` range before calling this) and the
    /// next frame must not reference them. Real hardware encoders invalidate
    /// just those references (NVENC `NvEncInvalidateRefFrames`, AMF/QSV
    /// equivalents) — a cheap P-frame instead of a full keyframe; per
    /// NVIDIA's docs the driver also invalidates frames *reconstructed from*
    /// a corrupt frame, and falls back to an intra frame itself if no valid
    /// reference remains. The default falls back to a full IDR, which always
    /// recovers (just not cheaply); encoders that can't do real invalidation
    /// should leave this as-is rather than reimplement it.
    fn invalidate_ref_timestamps(&mut self, _timestamps: &[u64]) {
        self.force_idr();
    }

    /// SPS/PPS (Annex-B) for the `CodecConfig` control message, if known.
    fn codec_config(&self) -> Option<Vec<u8>> {
        None
    }

    /// Change the target bitrate. Default no-op — encoders that can't
    /// reconfigure live (or don't have a meaningful bitrate concept, like
    /// `MockEncoder`'s bundled clip) simply ignore it; `server.rs`'s
    /// adaptive controller calls this unconditionally regardless of backend.
    fn set_bitrate(&mut self, _kbps: u32) {}

    /// Which codec `encode_next()`'s output is actually encoded as — may
    /// differ from what a client requested (e.g. AV1 unavailable on this
    /// GPU, silently fell back to H.264). Reported to the client via
    /// `HostMsg::SessionAccept.codec` instead of assuming the request was
    /// honored. Defaults to H.264 since every encoder except the direct
    /// NVENC path only ever produces that.
    fn codec(&self) -> Codec {
        Codec::H264
    }
}
