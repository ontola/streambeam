//! Control messages on stream S0 (client ↔ host).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoCaps {
    pub h264: bool,
    pub hevc: bool,
    pub av1: bool,
    pub max_width: u16,
    pub max_height: u16,
    pub max_fps: u8,
    /// Stable platform display identifier, or `None` for the host's primary
    /// display. Applied when the session starts.
    pub display_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioCaps {
    pub opus: bool,
    pub max_channels: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u8,
}

/// Input semantics exposed by the host OS for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostInputMode {
    /// Desktop-style pointer, wheel and keyboard injection.
    Mouse,
    /// Native absolute touch contacts, including multi-touch.
    Touch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        proto_version: u8,
        client_name: String,
        pair_proof: Option<[u8; 32]>,
    },
    SessionRequest {
        video: VideoCaps,
        audio: AudioCaps,
        bitrate_kbps: u32,
    },
    RequestIdr,
    /// Reference-Frame-Invalidation (RFI) recovery: the client lost video
    /// frame_ids `[first_frame, last_frame]` (inclusive, wrapping) but still
    /// holds a recent-enough known-good frame to reference instead — cheaper
    /// than `RequestIdr` since the host can encode a plain P-frame that skips
    /// these as references rather than a full keyframe. See
    /// `streambeam_core::reassemble::LossReport` for how the client decides
    /// between this and `RequestIdr`.
    InvalidateRefFrames {
        first_frame: u16,
        last_frame: u16,
    },
    SetBitrate {
        kbps: u32,
    },
    Ping {
        t_us: u64,
    },
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMsg {
    PairAck,
    PairReject {
        reason: String,
    },
    DisplayList {
        displays: Vec<DisplayInfo>,
        selected_id: u32,
    },
    SessionAccept {
        session_id: u64,
        codec: String,
        width: u16,
        height: u16,
        fps: u8,
        audio: AudioParams,
        input_mode: HostInputMode,
    },
    CodecConfig {
        data: Vec<u8>,
    },
    Rumble {
        pad: u8,
        low: u16,
        high: u16,
    },
    HostStats {
        capture_us: u32,
        encode_us: u32,
        send_queue: u16,
    },
    /// UTF-8 text clipboard update from the host. Only sent to an
    /// authenticated viewer; clients de-duplicate echoes locally.
    ClipboardText {
        text: String,
    },
    Pong {
        t_us: u64,
    },
    Bye,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame;

    #[test]
    fn client_msg_roundtrip() {
        let msg = ClientMsg::Hello {
            proto_version: 1,
            client_name: "devclient".into(),
            pair_proof: Some([7u8; 32]),
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<ClientMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn invalidate_ref_frames_roundtrip() {
        let msg = ClientMsg::InvalidateRefFrames {
            first_frame: 65_534,
            last_frame: 3,
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<ClientMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn host_msg_roundtrip() {
        let msg = HostMsg::PairReject {
            reason: "bad token".into(),
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<HostMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn display_list_roundtrip() {
        let msg = HostMsg::DisplayList {
            displays: vec![DisplayInfo {
                id: 42,
                name: "Built-in display".into(),
                width: 3024,
                height: 1964,
                primary: true,
            }],
            selected_id: 42,
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<HostMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn clipboard_roundtrip() {
        let msg = HostMsg::ClipboardText {
            text: "hello from host".into(),
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<HostMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
