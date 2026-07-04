use std::collections::{HashMap, HashSet};

use streambeam_proto::fec::{decode_frame, MediaShard, MAX_DATA_SHARDS};
use streambeam_proto::media::{MediaHeader, MEDIA_HEADER_LEN};
use streambeam_proto::ProtoError;

#[derive(Debug, Default)]
pub struct FrameStats {
    pub frames_complete: u64,
    pub frames_abandoned: u64,
    pub shards_received: u64,
    pub bytes_received: u64,
}

/// How many frame ids behind `latest` an incomplete bucket may lag before its
/// shards are abandoned. Keep in lockstep with docs/PROTOCOL.md's "Media
/// datagram" section — headroom over 2 to tolerate normal reordering/jitter.
const STALE_THRESHOLD: i16 = 4;

/// How many most-recently-completed frame ids we remember, purely to reject a
/// late duplicate shard of an already-delivered frame without re-emitting it.
/// Must stay well under `u16::MAX / 2` so wrap-aware distance math (used to
/// evict old entries once a full `frame_id` wrap happens) stays unambiguous.
const COMPLETED_WINDOW: u16 = 256;

/// Max number of frame buckets buffered concurrently while incomplete. Bounds
/// memory when a peer never completes frames (version skew, malformed
/// stream, or a flood of bogus datagrams) instead of buffering forever.
const MAX_INFLIGHT_FRAMES: usize = 16;

/// Wrap-aware signed distance: how far `id` is *behind* `latest` on the
/// circular `u16` frame-id space. Positive means behind, negative means
/// ahead (e.g. the not-yet-complete next frame while the current one lands).
fn wrap_distance(latest: u16, id: u16) -> i16 {
    latest.wrapping_sub(id) as i16
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
    k: u16,
    r: u16,
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

    /// Abandon buckets that are genuinely stale — behind `latest` by at least
    /// `STALE_THRESHOLD` frames. Uses a signed wrap distance so a bucket for a
    /// frame *ahead* of `latest` (e.g. `latest + 1`, mid-flight while
    /// `latest` completes) is never mistaken for a stale one.
    fn abandon_stale(&mut self, latest: u16) {
        let stale: Vec<u16> = self
            .buckets
            .keys()
            .copied()
            .filter(|id| wrap_distance(latest, *id) >= STALE_THRESHOLD)
            .collect();
        for id in stale {
            self.buckets.remove(&id);
            self.stats.frames_abandoned += 1;
        }
    }

    /// Evict the bucket furthest behind `latest` (by signed wrap distance) to
    /// make room for a new one, bounding memory when frames never complete.
    fn evict_oldest_bucket(&mut self, latest: u16) {
        if let Some(oldest) = self
            .buckets
            .keys()
            .copied()
            .max_by_key(|id| wrap_distance(latest, *id))
        {
            self.buckets.remove(&oldest);
            self.stats.frames_abandoned += 1;
        }
    }

    /// Forget completed-frame-id entries that have fallen out of the
    /// dedup window, so a `frame_id` reused after a full `u16` wrap is
    /// deliverable again instead of being permanently treated as a duplicate.
    fn prune_completed(&mut self, latest: u16) {
        self.completed
            .retain(|id| wrap_distance(latest, *id).unsigned_abs() < COMPLETED_WINDOW);
    }

    pub fn push_datagram(
        &mut self,
        datagram: &[u8],
    ) -> Result<Option<(u16, Vec<u8>)>, ProtoError> {
        let header = MediaHeader::decode(datagram)?;

        // Reject headers that can't possibly describe a real frame before
        // buffering anything for them. A version-skewed or hostile peer
        // could otherwise pin arbitrary amounts of memory on `k`/`frame_len`
        // values that will never validate in `decode_frame`.
        let k = usize::from(header.k);
        if k == 0 || k > MAX_DATA_SHARDS {
            return Err(ProtoError::Invalid {
                field: "k",
                value: header.k as u64,
            });
        }
        let total = k + usize::from(header.r);
        if usize::from(header.shard_index) >= total {
            return Err(ProtoError::Invalid {
                field: "shard_index",
                value: header.shard_index as u64,
            });
        }

        let payload = datagram
            .get(MEDIA_HEADER_LEN..)
            .ok_or(ProtoError::TooShort {
                need: MEDIA_HEADER_LEN + 1,
                got: datagram.len(),
            })?
            .to_vec();

        self.stats.shards_received += 1;
        self.stats.bytes_received += datagram.len() as u64;

        let shard = MediaShard { header, payload };
        let frame_id = shard.header.frame_id;

        if !self.buckets.contains_key(&frame_id) && self.buckets.len() >= MAX_INFLIGHT_FRAMES {
            self.evict_oldest_bucket(frame_id);
        }

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
                self.prune_completed(frame_id);
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
    use streambeam_proto::media::CHANNEL_VIDEO;

    fn push(asm: &mut FrameReassembler, shard: &MediaShard) -> Option<(u16, Vec<u8>)> {
        let mut dg = shard.header.encode().to_vec();
        dg.extend_from_slice(&shard.payload);
        asm.push_datagram(&dg).unwrap()
    }

    #[test]
    fn reassemble_from_datagrams() {
        let frame: Vec<u8> = (0..4000).map(|i| (i % 251) as u8).collect();
        let shards = encode_video_frame(1, &frame, true, 0, 1300).unwrap();
        let mut asm = FrameReassembler::default();
        let mut out = None;
        for shard in &shards {
            if let Some((_, f)) = push(&mut asm, shard) {
                out = Some(f);
            }
        }
        assert_eq!(out.unwrap(), frame);
        assert_eq!(asm.stats().frames_complete, 1);
    }

    /// Bug 1: the completed-frame dedup set must be pruned, not grown
    /// forever, or every frame_id after a full u16 wrap is (wrongly) treated
    /// as an already-delivered duplicate and reassembly stalls permanently.
    #[test]
    fn frames_keep_completing_after_frame_id_wraps() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;
        let mut completed_after_wrap = 0u32;

        for i in 0..70_000u32 {
            let frame_id = (i % 65536) as u16;
            let frame = vec![(i % 256) as u8; 4];
            let shards = encode_video_frame(frame_id, &frame, false, 0, mtu).unwrap();

            let mut delivered = None;
            for shard in &shards {
                if let Some((id, f)) = push(&mut asm, shard) {
                    delivered = Some((id, f));
                    break;
                }
            }
            let (id, f) = delivered
                .unwrap_or_else(|| panic!("frame {frame_id} (iteration {i}) never completed"));
            assert_eq!(id, frame_id);
            assert_eq!(f, frame);

            if i >= 65_536 {
                completed_after_wrap += 1;
            }
        }

        assert!(completed_after_wrap > 0);
        assert_eq!(asm.stats().frames_complete, 70_000);
    }

    /// Bug 2: completing frame N must not abandon a not-yet-complete bucket
    /// for frame N+1 that's simply mid-flight (out-of-order shard arrival).
    #[test]
    fn future_frame_not_abandoned_when_current_completes() {
        let mtu = 1300;
        let frame_n: Vec<u8> = vec![1u8; 4000];
        let frame_n1: Vec<u8> = vec![2u8; 4000];
        let shards_n = encode_video_frame(10, &frame_n, false, 0, mtu).unwrap();
        let shards_n1 = encode_video_frame(11, &frame_n1, false, 0, mtu).unwrap();
        let k1 = usize::from(shards_n1[0].header.k);
        assert!(k1 > 1, "test needs a multi-shard frame");

        let mut asm = FrameReassembler::default();

        // Deliver all but one data shard of frame 11 — still incomplete.
        for shard in shards_n1.iter().take(k1 - 1) {
            assert!(push(&mut asm, shard).is_none());
        }

        // Now complete frame 10, which triggers abandon_stale(10).
        let mut got10 = None;
        for shard in &shards_n {
            if let Some(result) = push(&mut asm, shard) {
                got10 = Some(result);
                break;
            }
        }
        let (id10, f10) = got10.expect("frame 10 should complete");
        assert_eq!(id10, 10);
        assert_eq!(f10, frame_n);
        assert_eq!(
            asm.stats().frames_abandoned,
            0,
            "frame 11's bucket must not be abandoned as stale while it's still ahead of latest"
        );

        // Finish frame 11 — it must still be able to complete.
        let (id11, f11) = push(&mut asm, &shards_n1[k1 - 1]).expect("frame 11 should complete");
        assert_eq!(id11, 11);
        assert_eq!(f11, frame_n1);
    }

    /// Bug 3a: a header with an absurd `k` or a `shard_index` outside
    /// `k + r` must be rejected at ingest, never buffered.
    #[test]
    fn garbage_header_rejected_without_buffering() {
        let mut asm = FrameReassembler::default();

        let huge_k = MediaHeader {
            channel: CHANNEL_VIDEO,
            flags: 0,
            frame_id: 1,
            shard_index: 0,
            k: 60_000,
            r: 0,
            frame_len: u32::MAX,
            capture_ts_us: 0,
        };
        let mut dg = huge_k.encode().to_vec();
        dg.extend_from_slice(&[0u8; 4]);
        match asm.push_datagram(&dg).unwrap_err() {
            ProtoError::Invalid { field, .. } => assert_eq!(field, "k"),
            other => panic!("expected Invalid(k), got {other:?}"),
        }
        assert!(asm.buckets.is_empty());

        let bad_index = MediaHeader {
            channel: CHANNEL_VIDEO,
            flags: 0,
            frame_id: 2,
            shard_index: 10,
            k: 4,
            r: 1,
            frame_len: 100,
            capture_ts_us: 0,
        };
        let mut dg = bad_index.encode().to_vec();
        dg.extend_from_slice(&[0u8; 4]);
        match asm.push_datagram(&dg).unwrap_err() {
            ProtoError::Invalid { field, .. } => assert_eq!(field, "shard_index"),
            other => panic!("expected Invalid(shard_index), got {other:?}"),
        }
        assert!(asm.buckets.is_empty());
        assert_eq!(asm.stats().shards_received, 0);
    }

    /// Bug 3b: incomplete-frame buckets must be capped, not buffered forever,
    /// when frames never complete (e.g. a mismatched/hostile stream).
    #[test]
    fn inflight_frame_buckets_are_bounded() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        for id in 0..(MAX_INFLIGHT_FRAMES as u16 + 20) {
            let frame = vec![id as u8; 4000]; // k > 1, so one shard never completes it.
            let shards = encode_video_frame(id, &frame, false, 0, mtu).unwrap();
            assert!(push(&mut asm, &shards[0]).is_none());
        }

        assert!(asm.buckets.len() <= MAX_INFLIGHT_FRAMES);
        assert!(asm.stats().frames_abandoned > 0);
    }
}
