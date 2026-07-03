//! Reed-Solomon frame sharding and recovery.

use reed_solomon_simd;

use crate::media::{self, MediaHeader, DEFAULT_FEC_RATIO, FLAG_KEYFRAME, FLAG_PARITY};
use crate::ProtoError;

/// Maximum data shards per frame (fits in u8 k/r fields with headroom).
pub const MAX_DATA_SHARDS: usize = 255;

/// One encoded shard: header + payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaShard {
    pub header: MediaHeader,
    pub payload: Vec<u8>,
}

/// Split `frame` into K data + R parity shards, each wrapped in a [`MediaShard`].
pub fn encode_frame(
    channel: u8,
    frame_id: u16,
    frame: &[u8],
    keyframe: bool,
    capture_ts_us: u32,
    max_datagram_size: usize,
    fec_ratio: f64,
) -> Result<Vec<MediaShard>, ProtoError> {
    let shard_len = media::shard_payload_len(max_datagram_size);
    if shard_len == 0 {
        return Err(ProtoError::Invalid {
            field: "max_datagram_size",
            value: max_datagram_size as u64,
        });
    }

    let k = frame.len().div_ceil(shard_len);
    if k == 0 {
        return Err(ProtoError::Invalid {
            field: "frame_len",
            value: 0,
        });
    }
    if k > MAX_DATA_SHARDS {
        return Err(ProtoError::FrameTooLarge {
            max: MAX_DATA_SHARDS,
            frame_len: frame.len(),
            shard_len,
        });
    }

    let r = media::parity_count(k, fec_ratio);
    let total = k + r;
    if total > u16::MAX as usize {
        return Err(ProtoError::FrameTooLarge {
            max: u16::MAX as usize,
            frame_len: frame.len(),
            shard_len,
        });
    }

    let mut data_shards = vec![vec![0u8; shard_len]; k];
    for (i, chunk) in frame.chunks(shard_len).enumerate() {
        data_shards[i][..chunk.len()].copy_from_slice(chunk);
    }

    let parity = if r > 0 {
        reed_solomon_simd::encode(k, r, data_shards.iter())?
    } else {
        Vec::new()
    };

    let flags_base = if keyframe { FLAG_KEYFRAME } else { 0 };
    let frame_len = frame.len() as u32;
    let k_u8 = k as u8;
    let r_u8 = r as u8;

    let mut out = Vec::with_capacity(total);
    for (idx, payload) in data_shards.into_iter().enumerate() {
        out.push(MediaShard {
            header: MediaHeader {
                channel,
                flags: flags_base,
                frame_id,
                shard_index: idx as u16,
                k: k_u8,
                r: r_u8,
                frame_len,
                capture_ts_us,
            },
            payload,
        });
    }
    for (i, payload) in parity.into_iter().enumerate() {
        out.push(MediaShard {
            header: MediaHeader {
                channel,
                flags: flags_base | FLAG_PARITY,
                frame_id,
                shard_index: (k + i) as u16,
                k: k_u8,
                r: r_u8,
                frame_len,
                capture_ts_us,
            },
            payload,
        });
    }
    Ok(out)
}

/// Recover the original frame bytes from any K of K+R received shards.
pub fn decode_frame(shards: &[MediaShard]) -> Result<Vec<u8>, ProtoError> {
    if shards.is_empty() {
        return Err(ProtoError::NotEnoughShards { have: 0, need: 1 });
    }

    let h0 = &shards[0].header;
    let k = usize::from(h0.k);
    let r = usize::from(h0.r);
    let frame_len = h0.frame_len as usize;
    let shard_len = shards[0].payload.len();

    if shard_len == 0 || shard_len % 2 != 0 {
        return Err(ProtoError::Invalid {
            field: "shard_len",
            value: shard_len as u64,
        });
    }

    for s in shards {
        if s.header.frame_id != h0.frame_id
            || s.header.k != h0.k
            || s.header.r != h0.r
            || s.header.frame_len != h0.frame_len
            || s.payload.len() != shard_len
        {
            return Err(ProtoError::Invalid {
                field: "shard_header_mismatch",
                value: 0,
            });
        }
    }

    if shards.len() < k {
        return Err(ProtoError::NotEnoughShards {
            have: shards.len(),
            need: k,
        });
    }

    let mut originals_map: Vec<(usize, &[u8])> = Vec::new();
    let mut recoveries: Vec<(usize, &[u8])> = Vec::new();
    for s in shards {
        let idx = usize::from(s.header.shard_index);
        if s.header.is_parity() {
            if idx < k {
                return Err(ProtoError::Invalid {
                    field: "parity_index",
                    value: idx as u64,
                });
            }
            recoveries.push((idx - k, &s.payload));
        } else if idx < k {
            originals_map.push((idx, &s.payload));
        } else {
            return Err(ProtoError::Invalid {
                field: "data_index",
                value: idx as u64,
            });
        }
    }

    let have_all_data = (0..k).all(|idx| originals_map.iter().any(|(i, _)| *i == idx));

    let mut frame = if have_all_data {
        let mut by_idx: Vec<Option<&[u8]>> = vec![None; k];
        for (idx, data) in &originals_map {
            by_idx[*idx] = Some(data);
        }
        let mut out = Vec::with_capacity(k * shard_len);
        for data in by_idx.into_iter().flatten() {
            out.extend_from_slice(data);
        }
        out
    } else {
        let restored = reed_solomon_simd::decode(k, r, originals_map.clone(), recoveries)?;
        let mut by_idx: Vec<Option<Vec<u8>>> = vec![None; k];
        for (idx, data) in originals_map {
            by_idx[idx] = Some(data.to_vec());
        }
        for (idx, data) in restored {
            by_idx[idx] = Some(data);
        }
        let mut out = Vec::with_capacity(k * shard_len);
        for idx in 0..k {
            out.extend_from_slice(
                by_idx[idx]
                    .as_deref()
                    .ok_or(ProtoError::NotEnoughShards {
                        have: shards.len(),
                        need: k,
                    })?,
            );
        }
        out
    };

    frame.truncate(frame_len);
    Ok(frame)
}

/// Convenience wrapper using the default FEC ratio.
pub fn encode_video_frame(
    frame_id: u16,
    frame: &[u8],
    keyframe: bool,
    capture_ts_us: u32,
    max_datagram_size: usize,
) -> Result<Vec<MediaShard>, ProtoError> {
    encode_frame(
        media::CHANNEL_VIDEO,
        frame_id,
        frame,
        keyframe,
        capture_ts_us,
        max_datagram_size,
        DEFAULT_FEC_RATIO,
    )
}

/// One 10 ms Opus packet on the audio channel (K=1, R=0, no RS framing).
pub fn encode_audio_packet(
    packet_id: u16,
    opus_frame: &[u8],
    capture_ts_us: u32,
) -> MediaShard {
    MediaShard {
        header: MediaHeader {
            channel: media::CHANNEL_AUDIO,
            flags: 0,
            frame_id: packet_id,
            shard_index: 0,
            k: 1,
            r: 0,
            frame_len: opus_frame.len() as u32,
            capture_ts_us,
        },
        payload: opus_frame.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    const MTU: usize = 1300;

    #[test]
    fn roundtrip_no_loss() {
        let frame: Vec<u8> = (0..5_000).map(|i| (i % 251) as u8).collect();
        let shards = encode_video_frame(7, &frame, true, 99, MTU).unwrap();
        let recovered = decode_frame(&shards).unwrap();
        assert_eq!(recovered, frame);
    }

    #[test]
    fn recover_with_data_loss() {
        let frame: Vec<u8> = (0..8_000).map(|i| (i % 251) as u8).collect();
        let shards = encode_video_frame(1, &frame, false, 0, MTU).unwrap();
        let k = usize::from(shards[0].header.k);
        // Drop first data shard; fill with parity until we have k shards.
        let kept: Vec<_> = shards
            .iter()
            .filter(|s| !s.header.is_parity() && s.header.shard_index != 0)
            .chain(shards.iter().filter(|s| s.header.is_parity()))
            .take(k)
            .cloned()
            .collect();
        assert_eq!(kept.len(), k);
        let recovered = decode_frame(&kept).unwrap();
        assert_eq!(recovered, frame);
    }

    #[test]
    fn random_loss_property() {
        let mut rng = rand::rng();
        for _ in 0..20 {
            let len = rng.random_range(100..20_000);
            let frame: Vec<u8> = (0..len).map(|_| rng.random()).collect();
            let shards = encode_video_frame(rng.random(), &frame, false, 0, MTU).unwrap();
            let k = usize::from(shards[0].header.k);
            let mut indices: Vec<usize> = (0..shards.len()).collect();
            // Keep exactly k random shards.
            for i in 0..shards.len() - k {
                let j = rng.random_range(i..shards.len());
                indices.swap(i, j);
            }
            let kept: Vec<_> = indices[..k]
                .iter()
                .map(|&i| shards[i].clone())
                .collect();
            let recovered = decode_frame(&kept).unwrap();
            assert_eq!(recovered, frame);
        }
    }
}
