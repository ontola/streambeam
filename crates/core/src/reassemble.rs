use std::collections::{HashMap, HashSet};

use streambeam_proto::fec::{decode_frame, MediaShard};
use streambeam_proto::media::MediaHeader;

#[derive(Debug, Default)]
pub struct FrameStats {
    pub frames_complete: u64,
    pub frames_abandoned: u64,
    pub shards_received: u64,
    pub bytes_received: u64,
}

/// Collects media shards and returns full frame bytes once K-of-K+R recovery succeeds.
#[derive(Debug, Default)]
pub struct FrameReassembler {
    buckets: HashMap<u16, FrameBucket>,
    completed: HashSet<u16>,
    stats: FrameStats,
    idr_requested: bool,
}

#[derive(Debug)]
struct FrameBucket {
    k: u8,
    r: u8,
    frame_len: u32,
    shards: Vec<MediaShard>,
}

impl FrameReassembler {
    pub fn stats(&self) -> &FrameStats {
        &self.stats
    }

    pub fn take_idr_request(&mut self) -> bool {
        std::mem::replace(&mut self.idr_requested, false)
    }

    fn abandon_stale(&mut self, latest: u16) {
        let stale: Vec<u16> = self
            .buckets
            .keys()
            .copied()
            .filter(|id| latest.wrapping_sub(*id) >= 4)
            .collect();
        for id in stale {
            self.buckets.remove(&id);
            self.stats.frames_abandoned += 1;
        }
    }

    pub fn push_datagram(
        &mut self,
        datagram: &[u8],
    ) -> Result<Option<(u16, Vec<u8>)>, streambeam_proto::ProtoError> {
        let header = MediaHeader::decode(datagram)?;
        let payload = datagram
            .get(16..)
            .ok_or(streambeam_proto::ProtoError::TooShort {
                need: 17,
                got: datagram.len(),
            })?
            .to_vec();

        self.stats.shards_received += 1;
        self.stats.bytes_received += datagram.len() as u64;

        let shard = MediaShard { header, payload };
        let frame_id = shard.header.frame_id;

        let bucket = self.buckets.entry(frame_id).or_insert_with(|| FrameBucket {
            k: shard.header.k,
            r: shard.header.r,
            frame_len: shard.header.frame_len,
            shards: Vec::new(),
        });

        if bucket.k != shard.header.k
            || bucket.r != shard.header.r
            || bucket.frame_len != shard.header.frame_len
        {
            self.buckets.remove(&frame_id);
            self.stats.frames_abandoned += 1;
            return Ok(None);
        }

        if bucket
            .shards
            .iter()
            .any(|s| s.header.shard_index == shard.header.shard_index)
        {
            return Ok(None);
        }
        bucket.shards.push(shard);

        let need = usize::from(bucket.k);
        if bucket.shards.len() < need {
            return Ok(None);
        }

        let shards = std::mem::take(&mut bucket.shards);
        self.buckets.remove(&frame_id);

        match decode_frame(&shards) {
            Ok(frame) => {
                if !self.completed.insert(frame_id) {
                    return Ok(None);
                }
                self.stats.frames_complete += 1;
                self.abandon_stale(frame_id);
                Ok(Some((frame_id, frame)))
            }
            Err(e) => {
                self.stats.frames_abandoned += 1;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streambeam_proto::fec::encode_video_frame;

    #[test]
    fn reassemble_from_datagrams() {
        let frame: Vec<u8> = (0..4000).map(|i| (i % 251) as u8).collect();
        let shards = encode_video_frame(1, &frame, true, 0, 1300).unwrap();
        let mut asm = FrameReassembler::default();
        let mut out = None;
        for shard in &shards {
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            if let Some((_, f)) = asm.push_datagram(&dg).unwrap() {
                out = Some(f);
            }
        }
        assert_eq!(out.unwrap(), frame);
        assert_eq!(asm.stats().frames_complete, 1);
    }
}
