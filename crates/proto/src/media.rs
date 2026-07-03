//! 16-byte little-endian media datagram header.

use crate::ProtoError;

pub const MEDIA_HEADER_LEN: usize = 16;

pub const CHANNEL_VIDEO: u8 = 0;
pub const CHANNEL_AUDIO: u8 = 1;

pub const FLAG_KEYFRAME: u8 = 1 << 0;
pub const FLAG_PARITY: u8 = 1 << 1;

/// Hard ceiling for shard payload bytes (see PROTOCOL.md).
pub const MAX_SHARD_PAYLOAD: usize = 1200;

/// Default Reed-Solomon redundancy ratio for video frames.
pub const DEFAULT_FEC_RATIO: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub channel: u8,
    pub flags: u8,
    pub frame_id: u16,
    pub shard_index: u16,
    pub k: u8,
    pub r: u8,
    pub frame_len: u32,
    pub capture_ts_us: u32,
}

impl MediaHeader {
    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }

    pub fn is_parity(&self) -> bool {
        self.flags & FLAG_PARITY != 0
    }

    pub fn total_shards(&self) -> usize {
        usize::from(self.k) + usize::from(self.r)
    }

    pub fn encode(&self) -> [u8; MEDIA_HEADER_LEN] {
        let mut buf = [0u8; MEDIA_HEADER_LEN];
        buf[0] = self.channel;
        buf[1] = self.flags;
        buf[2..4].copy_from_slice(&self.frame_id.to_le_bytes());
        buf[4..6].copy_from_slice(&self.shard_index.to_le_bytes());
        buf[6] = self.k;
        buf[7] = self.r;
        buf[8..12].copy_from_slice(&self.frame_len.to_le_bytes());
        buf[12..16].copy_from_slice(&self.capture_ts_us.to_le_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < MEDIA_HEADER_LEN {
            return Err(ProtoError::TooShort {
                need: MEDIA_HEADER_LEN,
                got: buf.len(),
            });
        }
        Ok(Self {
            channel: buf[0],
            flags: buf[1],
            frame_id: u16::from_le_bytes([buf[2], buf[3]]),
            shard_index: u16::from_le_bytes([buf[4], buf[5]]),
            k: buf[6],
            r: buf[7],
            frame_len: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            capture_ts_us: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        })
    }
}

/// Compute per-shard payload length from QUIC datagram MTU.
pub fn shard_payload_len(max_datagram_size: usize) -> usize {
    let raw = max_datagram_size.saturating_sub(MEDIA_HEADER_LEN);
    raw.min(MAX_SHARD_PAYLOAD) & !1
}

/// Number of parity shards for `k` data shards at `fec_ratio`.
pub fn parity_count(k: usize, fec_ratio: f64) -> usize {
    if k == 0 {
        return 0;
    }
    ((k as f64) * fec_ratio).ceil() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = MediaHeader {
            channel: CHANNEL_VIDEO,
            flags: FLAG_KEYFRAME,
            frame_id: 42,
            shard_index: 3,
            k: 8,
            r: 2,
            frame_len: 9_876,
            capture_ts_us: 1_234_567,
        };
        let decoded = MediaHeader::decode(&h.encode()).unwrap();
        assert_eq!(h, decoded);
    }

    #[test]
    fn shard_len_even_and_capped() {
        assert_eq!(shard_payload_len(1400), 1200);
        assert_eq!(shard_payload_len(500), 484);
        assert_eq!(shard_payload_len(16), 0);
    }

    #[test]
    fn parity_count_ceil() {
        assert_eq!(parity_count(10, 0.15), 2);
        assert_eq!(parity_count(1, 0.15), 1);
        assert_eq!(parity_count(0, 0.15), 0);
    }
}
