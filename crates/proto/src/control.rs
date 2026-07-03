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
    SessionAccept {
        session_id: u64,
        codec: String,
        width: u16,
        height: u16,
        fps: u8,
        audio: AudioParams,
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
    fn host_msg_roundtrip() {
        let msg = HostMsg::PairReject {
            reason: "bad token".into(),
        };
        let bytes = frame::encode_frame(&msg).unwrap();
        let (decoded, _) = frame::decode_frame::<HostMsg>(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
