use std::collections::{HashMap, HashSet};

use streambeam_proto::ProtoError;
use streambeam_proto::fec::{MAX_DATA_SHARDS, MediaShard, decode_frame};
use streambeam_proto::media::{MEDIA_HEADER_LEN, MediaHeader};

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

// A bucket that is `STALE_THRESHOLD` frames behind is only abandoned if it also
// took no new shard since the previous staleness check — see `abandon_stale`.
//
// Frame-id distance alone is not a staleness test: it assumes a frame's shards
// all arrive within a few frame intervals, which is false for a large keyframe
// at a high frame rate. A 4K keyframe is ~400 paced shards, while the host keeps
// emitting tiny single-shard duplicate frames to honour a 120fps target — so
// four newer frames routinely complete while the keyframe is still legitimately
// filling, and distance alone threw it away mid-flight. The client then had no
// reference frame, latched `awaiting_keyframe`, skipped every P-frame and asked
// for another IDR, which met the same fate: bytes flowing, nothing rendered — a
// black screen at 4K while 1080p (~50-shard keyframes) worked. Diagnosed
// 2026-08-17 against a real 4K host.
//
// "Received a shard since the last check" separates the two cases without a
// clock and without delaying recovery: a frame whose shards were genuinely lost
// goes quiet immediately, so it is still abandoned on the same completion it
// always was, keeping the loss report (and the IDR it asks for) prompt.

/// How many most-recently-completed frame ids we remember, purely to reject a
/// late duplicate shard of an already-delivered frame without re-emitting it.
/// Must stay well under `u16::MAX / 2` so wrap-aware distance math (used to
/// evict old entries once a full `frame_id` wrap happens) stays unambiguous.
const COMPLETED_WINDOW: u16 = 256;

/// Max number of frame buckets buffered concurrently while incomplete. Bounds
/// memory when a peer never completes frames (version skew, malformed
/// stream, or a flood of bogus datagrams) instead of buffering forever.
const MAX_INFLIGHT_FRAMES: usize = 16;

/// How many frame_ids past the last known-good reference the encoder is
/// assumed to still hold in its reference window. Mirrors Moonlight's RFI
/// heuristic: a loss run within this window can plausibly be recovered
/// surgically (`LossReport::Invalidate` — the host skips the lost ids as
/// references for its next frame, a cheap P-frame). A run wider than this —
/// or no known-good reference at all — means the encoder has almost
/// certainly aged that reference out of its window already, so a surgical
/// fix can't work and a full keyframe (`LossReport::NeedIdr`) is requested
/// instead. 32 is a generous guess (roughly a second of reference headroom
/// at 30fps) with no hardware encoder yet to measure a real DPB depth
/// against; revisit once M2 lands and exposes the actual value.
const REF_WINDOW: u16 = 32;

/// Wrap-aware signed distance: how far `id` is *behind* `latest` on the
/// circular `u16` frame-id space. Positive means behind, negative means
/// ahead (e.g. the not-yet-complete next frame while the current one lands).
fn wrap_distance(latest: u16, id: u16) -> i16 {
    latest.wrapping_sub(id) as i16
}

/// What the client should tell the host after it gives up on recovering
/// video frame(s), so the host can recover cheaply instead of always forcing
/// a full keyframe. See `REF_WINDOW` for the decision heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossReport {
    /// Contiguous lost `frame_id` run `[first_frame, last_frame]` (inclusive,
    /// wrapping). The host's next encoded frame should not reference any of
    /// these — see `ClientMsg::InvalidateRefFrames`.
    Invalidate { first_frame: u16, last_frame: u16 },
    /// No usable recent reference (nothing has completed yet, or the loss
    /// run is wider than `REF_WINDOW`) — ask for a full refresh instead.
    NeedIdr,
}

/// A loss run accumulated since the last `take_loss_report`, plus the
/// reference frame the client still held *at the moment the run started*.
///
/// `reference` is frozen at first-loss time rather than re-read live from
/// `last_completed`, because later (higher) frame_ids can keep completing
/// over FEC — their bytes arrive fine — even though they're undecodable
/// without the frame(s) already reported lost. Re-reading a since-advanced
/// `last_completed` would put the "reference" *ahead* of the loss it's
/// supposed to precede, producing a backwards range.
#[derive(Debug, Clone, Copy)]
struct PendingLoss {
    reference: Option<u16>,
    first: u16,
    last: u16,
}

/// Collects media shards and returns full frame bytes once K-of-K+R recovery succeeds.
#[derive(Debug, Default)]
pub struct FrameReassembler {
    buckets: HashMap<u16, FrameBucket>,
    completed: HashSet<u16>,
    stats: FrameStats,
    /// Highest completed `frame_id` seen so far (wrap-aware) — the client's
    /// last known-good reference frame.
    last_completed: Option<u16>,
    pending_loss: Option<PendingLoss>,
}

#[derive(Debug)]
struct FrameBucket {
    k: u16,
    r: u16,
    frame_len: u32,
    shards: Vec<MediaShard>,
    /// Whether this bucket took a shard since the last staleness check — how a
    /// bucket that is still filling is told apart from one whose shards stopped
    /// arriving (see `abandon_stale`).
    took_shard_since_check: bool,
}

impl FrameReassembler {
    pub fn stats(&self) -> &FrameStats {
        &self.stats
    }

    /// Take the pending loss report, if any frame was abandoned since the
    /// last call. See `LossReport` / `REF_WINDOW` for the decision logic.
    pub fn take_loss_report(&mut self) -> Option<LossReport> {
        let pending = self.pending_loss.take()?;
        Some(match pending.reference {
            None => LossReport::NeedIdr,
            Some(last_good) => {
                // `last_good` (frozen at first-loss time — see `PendingLoss`)
                // normally precedes the lost range. But frames *after* a lost
                // one can complete independently over FEC (each frame's shard
                // group is decoded on its own) before the lost one is finally
                // evicted as stale, which can leave `last_good` numerically
                // *ahead* of `pending.first` — that says nothing about
                // whether the lost frame(s) are actually recoverable, so
                // don't let it flip the report to `NeedIdr`. Fall back to
                // treating the frame right before the loss run as the
                // reference instead: the most conservative point nameable
                // without a decode-dependency model, which this
                // network-layer reassembler doesn't have.
                let last_good = if wrap_distance(pending.first, last_good) > 0 {
                    last_good
                } else {
                    pending.first.wrapping_sub(1)
                };
                // How far ahead the newest lost frame is of that reference.
                // `wrap_distance(a, b)` returns `a - b` wrap-aware, so this is
                // positive exactly when `last_good` precedes `pending.last`
                // (guaranteed now by construction, barring a `pending.last`
                // that's somehow behind `pending.first`, which `record_loss`
                // never produces).
                let gap = wrap_distance(pending.last, last_good);
                if gap <= 0 || gap as u16 > REF_WINDOW {
                    LossReport::NeedIdr
                } else {
                    LossReport::Invalidate {
                        first_frame: last_good.wrapping_add(1),
                        last_frame: pending.last,
                    }
                }
            }
        })
    }

    /// Record that `id` was abandoned without completing, folding it into the
    /// in-progress loss run (or starting a new one, snapshotting the current
    /// `last_completed` as that run's reference — see `PendingLoss`).
    fn record_loss(&mut self, id: u16) {
        match &mut self.pending_loss {
            None => {
                self.pending_loss = Some(PendingLoss {
                    reference: self.last_completed,
                    first: id,
                    last: id,
                });
            }
            Some(pending) => {
                if wrap_distance(pending.first, id) > 0 {
                    pending.first = id;
                }
                if wrap_distance(id, pending.last) > 0 {
                    pending.last = id;
                }
            }
        }
    }

    /// Update the last known-good reference if `id` is newer (wrap-aware)
    /// than what we'd already seen.
    fn note_completed(&mut self, id: u16) {
        let is_newer = match self.last_completed {
            None => true,
            Some(prev) => wrap_distance(id, prev) > 0,
        };
        if is_newer {
            self.last_completed = Some(id);
        }
    }

    /// Abandon buckets that are genuinely stale — behind `latest` by at least
    /// `STALE_THRESHOLD` frames. Uses a signed wrap distance so a bucket for a
    /// frame *ahead* of `latest` (e.g. `latest + 1`, mid-flight while
    /// `latest` completes) is never mistaken for a stale one.
    ///
    /// Called *before* `note_completed(latest)` updates `last_completed`, so
    /// any loss recorded here still snapshots the reference frame as it stood
    /// prior to `latest` — see `PendingLoss`.
    fn abandon_stale(&mut self, latest: u16) {
        let stale: Vec<u16> = self
            .buckets
            .iter()
            .filter(|(id, bucket)| {
                wrap_distance(latest, **id) >= STALE_THRESHOLD && !bucket.took_shard_since_check
            })
            .map(|(id, _)| *id)
            .collect();
        for bucket in self.buckets.values_mut() {
            bucket.took_shard_since_check = false;
        }
        for id in stale {
            self.buckets.remove(&id);
            self.stats.frames_abandoned += 1;
            self.record_loss(id);
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
            self.record_loss(oldest);
        }
    }

    /// Forget completed-frame-id entries that have fallen out of the
    /// dedup window, so a `frame_id` reused after a full `u16` wrap is
    /// deliverable again instead of being permanently treated as a duplicate.
    fn prune_completed(&mut self, latest: u16) {
        self.completed
            .retain(|id| wrap_distance(latest, *id).unsigned_abs() < COMPLETED_WINDOW);
    }

    pub fn push_datagram(&mut self, datagram: &[u8]) -> Result<Option<(u16, Vec<u8>)>, ProtoError> {
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

        // A late shard for a frame_id we already fully delivered — most
        // commonly a redundant FEC parity shard that simply wasn't needed
        // (the frame completed from K data shards before all R parity
        // shards arrived) but arrives anyway on a healthy link. Drop it
        // before it can create a phantom bucket: `or_insert_with` below has
        // no idea `frame_id` was already completed and removed from
        // `buckets`, so without this check a harmless late shard would
        // start a new incomplete bucket that later gets genuinely (but
        // wrongly) abandoned as if the frame were lost.
        if self.completed.contains(&frame_id) {
            return Ok(None);
        }

        if !self.buckets.contains_key(&frame_id) && self.buckets.len() >= MAX_INFLIGHT_FRAMES {
            self.evict_oldest_bucket(frame_id);
        }

        let bucket = self.buckets.entry(frame_id).or_insert_with(|| FrameBucket {
            k: shard.header.k,
            r: shard.header.r,
            frame_len: shard.header.frame_len,
            shards: Vec::new(),
            // Starts unset: a frame that receives one shard and then nothing is
            // genuinely lost and must still be abandoned on the next check, as
            // before. A large frame flips this the moment its second shard
            // lands, which is what protects it while it is still filling.
            took_shard_since_check: false,
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
        // Only shards *after* the first count as "still filling" — see the
        // flag's initialiser for why a one-shard-then-silence frame must stay
        // abandonable on the very next check.
        let was_filling = !bucket.shards.is_empty();
        bucket.shards.push(shard);
        bucket.took_shard_since_check |= was_filling;

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
                self.note_completed(frame_id);
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

    /// A big keyframe still taking shards must survive newer frames completing
    /// past it. This is the 4K black screen of 2026-08-17: a ~400-shard
    /// keyframe was abandoned mid-fill because four tiny duplicate frames
    /// (120fps target) completed while it was still arriving, so the client
    /// never got a reference frame and rendered nothing at all.
    #[test]
    fn actively_filling_bucket_survives_newer_completions() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        // A large keyframe: many shards, delivered a few at a time.
        let keyframe = vec![7u8; 200_000];
        let key_shards = encode_video_frame(0, &keyframe, true, 0, mtu).unwrap();
        assert!(key_shards.len() > STALE_THRESHOLD as usize * 4);
        for shard in key_shards.iter().take(3) {
            assert!(push(&mut asm, shard).is_none());
        }

        // Newer single-shard frames complete instantly, running `latest` well
        // past the keyframe's id (the duplicate-frame cadence at high fps).
        for id in 1..=(STALE_THRESHOLD as u16 + 3) {
            let small = vec![id as u8; 200];
            let shards = encode_video_frame(id, &small, false, 0, mtu).unwrap();
            assert!(push(&mut asm, &shards[0]).is_some(), "frame {id} should complete");
            // Keep the keyframe visibly active between completions.
            let idx = 3 + id as usize;
            if idx < key_shards.len() {
                assert!(push(&mut asm, &key_shards[idx]).is_none());
            }
        }

        assert!(
            asm.buckets.contains_key(&0),
            "keyframe bucket was abandoned while still receiving shards"
        );

        // Delivering the rest still completes it.
        let mut completed = None;
        for shard in key_shards.iter().skip(1) {
            if let Some((id, frame)) = push(&mut asm, shard) {
                completed = Some((id, frame));
                break;
            }
        }
        let (id, frame) = completed.expect("keyframe must complete once its shards arrive");
        assert_eq!(id, 0);
        assert_eq!(frame, keyframe);
    }

    /// The activity rule must not keep genuinely lost frames forever: a bucket
    /// whose shards stopped arriving is still abandoned on the same completion
    /// as before, so the loss report asking for a fresh keyframe stays prompt.
    #[test]
    fn quiet_bucket_behind_latest_is_still_abandoned() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        let lost = vec![1u8; 4000]; // needs several shards; only one ever arrives
        let lost_shards = encode_video_frame(0, &lost, false, 0, mtu).unwrap();
        assert!(push(&mut asm, &lost_shards[0]).is_none());

        for id in 1..=(STALE_THRESHOLD as u16) {
            let small = vec![id as u8; 200];
            let shards = encode_video_frame(id, &small, false, 0, mtu).unwrap();
            push(&mut asm, &shards[0]);
        }

        assert!(
            !asm.buckets.contains_key(&0),
            "a frame that stopped receiving shards must still be abandoned"
        );
        assert!(asm.stats().frames_abandoned > 0);
        assert!(asm.take_loss_report().is_some(), "loss must still be reported");
    }

    /// RFI (b): losing frames before anything has ever completed leaves no
    /// known-good reference to invalidate around — must report `NeedIdr`.
    #[test]
    fn loss_before_any_completion_reports_need_idr() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        assert!(asm.take_loss_report().is_none());

        // Push only a first (incomplete) shard for enough frame_ids to make
        // the in-flight cap start evicting — nothing ever completes, so
        // `last_completed` stays `None` throughout.
        for id in 0..(MAX_INFLIGHT_FRAMES as u16 + 3) {
            let frame = vec![id as u8; 4000];
            let shards = encode_video_frame(id, &frame, false, 0, mtu).unwrap();
            assert!(push(&mut asm, &shards[0]).is_none());
        }

        assert_eq!(asm.take_loss_report(), Some(LossReport::NeedIdr));
    }

    /// RFI (a): a couple of middle frames abandoned while there's a recent
    /// known-good reference must report a wrap-safe `Invalidate` range.
    #[test]
    fn abandoned_frames_with_recent_reference_report_invalidate_range() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        // Frame 10 completes fully — this becomes the known-good reference.
        let frame10 = vec![10u8; 4000];
        let shards10 = encode_video_frame(10, &frame10, false, 0, mtu).unwrap();
        let mut got10 = None;
        for shard in &shards10 {
            if let Some(r) = push(&mut asm, shard) {
                got10 = Some(r);
                break;
            }
        }
        assert!(got10.is_some());
        assert!(asm.take_loss_report().is_none());

        // Frames 11 and 12 each get exactly one (of several) shards — buckets
        // exist but never complete. Frames 13-15 never get any datagram at
        // all (no bucket ever created — abandon_stale only evicts *existing*
        // buckets, so a total blackout for those ids doesn't register here).
        for id in [11u16, 12u16] {
            let frame = vec![id as u8; 4000];
            let shards = encode_video_frame(id, &frame, false, 0, mtu).unwrap();
            assert!(shards.len() > 1, "test needs a multi-shard frame");
            assert!(push(&mut asm, &shards[0]).is_none());
        }

        // Frame 16 completes: wrap_distance(16, 11) = 5 and
        // wrap_distance(16, 12) = 4, both >= STALE_THRESHOLD — both buckets
        // get abandoned in this single call.
        let frame16 = vec![16u8; 4000];
        let shards16 = encode_video_frame(16, &frame16, false, 0, mtu).unwrap();
        let mut got16 = None;
        for shard in &shards16 {
            if let Some(r) = push(&mut asm, shard) {
                got16 = Some(r);
                break;
            }
        }
        assert!(got16.is_some());

        assert_eq!(
            asm.take_loss_report(),
            Some(LossReport::Invalidate {
                first_frame: 11,
                last_frame: 12,
            })
        );
        // Reported once — a second call finds nothing new pending.
        assert!(asm.take_loss_report().is_none());
    }

    /// RFI (c): a loss run wider than `REF_WINDOW` past the last known-good
    /// reference means the encoder has almost certainly aged that reference
    /// out of its window already — must fall back to `NeedIdr`.
    #[test]
    fn loss_run_wider_than_ref_window_reports_need_idr() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        // Frame 0 completes — the only (soon frozen) known-good reference.
        let frame0 = vec![0u8; 4000];
        let shards0 = encode_video_frame(0, &frame0, false, 0, mtu).unwrap();
        for shard in &shards0 {
            if push(&mut asm, shard).is_some() {
                break;
            }
        }

        // A frame well past REF_WINDOW gets a partial (incomplete) bucket.
        let lost_id = REF_WINDOW + 8;
        let frame_lost = vec![lost_id as u8; 4000];
        let shards_lost = encode_video_frame(lost_id, &frame_lost, false, 0, mtu).unwrap();
        assert!(shards_lost.len() > 1);
        assert!(push(&mut asm, &shards_lost[0]).is_none());

        // A later frame completes and triggers abandon_stale on it.
        let trigger_id = lost_id + STALE_THRESHOLD as u16;
        let frame_trigger = vec![trigger_id as u8; 4000];
        let shards_trigger = encode_video_frame(trigger_id, &frame_trigger, false, 0, mtu).unwrap();
        for shard in &shards_trigger {
            if push(&mut asm, shard).is_some() {
                break;
            }
        }

        assert_eq!(asm.take_loss_report(), Some(LossReport::NeedIdr));
    }

    /// RFI (d): the lost-range computation must be correct (and not panic)
    /// across the u16 `frame_id` wrap boundary.
    #[test]
    fn invalidate_range_computed_across_u16_wrap() {
        let mut asm = FrameReassembler::default();
        let mtu = 1300;

        // Frame 65535 completes — the known-good reference, right at the
        // top of the u16 range.
        let frame_max = vec![7u8; 4000];
        let shards_max = encode_video_frame(u16::MAX, &frame_max, false, 0, mtu).unwrap();
        for shard in &shards_max {
            if push(&mut asm, shard).is_some() {
                break;
            }
        }

        // Frame 0 (just past the wrap) gets a partial bucket that never completes.
        let frame0 = vec![9u8; 4000];
        let shards0 = encode_video_frame(0, &frame0, false, 0, mtu).unwrap();
        assert!(shards0.len() > 1);
        assert!(push(&mut asm, &shards0[0]).is_none());

        // Frame 4 completes: wrap_distance(4, 0) == 4 >= STALE_THRESHOLD, so
        // the frame-0 bucket is abandoned here.
        let frame4 = vec![11u8; 4000];
        let shards4 = encode_video_frame(4, &frame4, false, 0, mtu).unwrap();
        for shard in &shards4 {
            if push(&mut asm, shard).is_some() {
                break;
            }
        }

        assert_eq!(
            asm.take_loss_report(),
            Some(LossReport::Invalidate {
                first_frame: 0, // 65535 + 1 wraps to 0
                last_frame: 0,
            })
        );
    }
}
