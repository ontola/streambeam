//! Decides which reassembled frames are worth handing to the platform decoder.
//!
//! This policy used to live inline in the client's receive loop, tangled up with
//! QUIC reads and the Dart channel hop, which made it effectively untestable —
//! and it has broken three separate times, each time as a black or near-frozen
//! picture that no test could catch:
//!
//! - 2026-07-19: a recovery keyframe arriving in a large batch re-armed the
//!   skip latch that it had just cleared → one rendered frame per IDR, a ~5s
//!   slideshow.
//! - 2026-08-17: a single greedy drain batch was mistaken for a backlog, so
//!   **96%** of perfectly-received frames were discarded with zero packet loss
//!   (measured `fed 75 / skipped 1658`).
//!
//! Both are policy bugs, not I/O bugs, so the policy is pure here and covered by
//! the tests at the bottom of this file.
//!
//! # Why skipping is dangerous at all
//!
//! Frames form a reference chain: feeding a P-frame whose references were
//! skipped decodes "successfully" into progressively worse garbage rather than
//! erroring — indistinguishable from corruption. So once any P-frame is skipped
//! the gate must keep skipping until a keyframe restores the chain
//! ([`FrameGate::awaiting_keyframe`]). That makes skipping expensive: it forces a
//! keyframe, and at 4K a keyframe is ~400 KB. Skip only when a backlog is real.

/// What to do with one completed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Hand it to the decoder.
    Feed,
    /// Drop it — its references are already gone, or we're catching up.
    Skip,
}

/// How many consecutive oversized drains count as a real backlog.
///
/// One big batch is normal: the receive loop drains greedily on purpose, and at
/// a high frame rate (with the host emitting a duplicate frame per tick) a
/// single batch routinely exceeds any fixed size. Only a backlog that persists
/// across drains means decode is actually losing ground.
const BEHIND_DRAINS_BEFORE_SKIP: u32 = 4;

/// Frame-feeding policy for one session. Construct with the negotiated fps.
#[derive(Debug)]
pub struct FrameGate {
    behind_threshold: u32,
    consecutive_behind: u32,
    awaiting_keyframe: bool,
    fed: u64,
    skipped: u64,
}

impl FrameGate {
    /// `fps` is the session's negotiated frame rate; the backlog threshold
    /// scales with it so a 30fps stream isn't more trigger-happy than a 120fps
    /// one. Floored at 10, the value proven on Android before fps scaling.
    pub fn new(fps: u8) -> Self {
        Self {
            behind_threshold: (u32::from(fps) / 4).max(10),
            consecutive_behind: 0,
            awaiting_keyframe: false,
            fed: 0,
            skipped: 0,
        }
    }

    /// True while waiting for a keyframe to restore the reference chain.
    pub fn awaiting_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }

    pub fn fed(&self) -> u64 {
        self.fed
    }

    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Decide one drain batch, given each completed frame's keyframe flag (in
    /// arrival order). Returns a decision per frame.
    ///
    /// A keyframe is always fed and always clears the latch, and — critically —
    /// clears it *for the rest of this batch*, so the P-frames that arrive right
    /// behind a recovery keyframe are fed rather than re-arming the latch that
    /// keyframe just cleared.
    pub fn decide(&mut self, keyframes: &[bool]) -> Vec<Decision> {
        if keyframes.len() as u32 > self.behind_threshold {
            self.consecutive_behind += 1;
        } else {
            self.consecutive_behind = 0;
        }
        let backlogged = self.consecutive_behind >= BEHIND_DRAINS_BEFORE_SKIP;
        let mut resync = self.awaiting_keyframe || backlogged;

        let mut out = Vec::with_capacity(keyframes.len());
        for &keyframe in keyframes {
            if keyframe {
                resync = false;
                self.awaiting_keyframe = false;
                self.fed += 1;
                out.push(Decision::Feed);
            } else if resync {
                self.awaiting_keyframe = true;
                self.skipped += 1;
                out.push(Decision::Skip);
            } else {
                self.fed += 1;
                out.push(Decision::Feed);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: usize) -> Vec<bool> {
        vec![false; n]
    }

    /// The everyday case: small batches at a high frame rate must all be fed.
    #[test]
    fn healthy_cadence_feeds_everything() {
        let mut gate = FrameGate::new(120);
        for _ in 0..500 {
            assert!(gate.decide(&p(2)).iter().all(|d| *d == Decision::Feed));
        }
        assert_eq!(gate.skipped(), 0);
        assert!(!gate.awaiting_keyframe());
    }

    /// Regression for 2026-08-17: one oversized greedy drain is not a backlog.
    /// This is the bug that discarded 96% of frames with zero packet loss.
    #[test]
    fn single_oversized_drain_does_not_trip_skipping() {
        let mut gate = FrameGate::new(120);
        let decisions = gate.decide(&p(64));
        assert!(decisions.iter().all(|d| *d == Decision::Feed));
        assert_eq!(gate.skipped(), 0);
        assert!(!gate.awaiting_keyframe());
    }

    /// ...and neither do a few of them interleaved with normal ones.
    #[test]
    fn intermittent_bursts_do_not_trip_skipping() {
        let mut gate = FrameGate::new(120);
        for _ in 0..20 {
            gate.decide(&p(64));
            gate.decide(&p(1));
        }
        assert_eq!(gate.skipped(), 0);
    }

    /// A genuinely sustained backlog still engages skipping — that's the whole
    /// point of the mechanism.
    #[test]
    fn sustained_backlog_engages_skipping() {
        let mut gate = FrameGate::new(120);
        let mut saw_skip = false;
        for _ in 0..BEHIND_DRAINS_BEFORE_SKIP + 1 {
            if gate.decide(&p(64)).contains(&Decision::Skip) {
                saw_skip = true;
            }
        }
        assert!(saw_skip, "a persistent backlog must eventually skip");
        assert!(gate.awaiting_keyframe());
    }

    /// Regression for 2026-07-19: the P-frames arriving in the same batch as a
    /// recovery keyframe must be fed, not re-arm the latch it just cleared.
    /// Getting this wrong produced one frame per IDR — a ~5s slideshow.
    #[test]
    fn keyframe_resumes_feeding_for_the_rest_of_its_batch() {
        let mut gate = FrameGate::new(120);
        for _ in 0..BEHIND_DRAINS_BEFORE_SKIP {
            gate.decide(&p(64));
        }
        assert!(gate.awaiting_keyframe());

        // [P, P, K, P, P] — skip until the keyframe, feed it and everything after.
        let decisions = gate.decide(&[false, false, true, false, false]);
        assert_eq!(
            decisions,
            vec![
                Decision::Skip,
                Decision::Skip,
                Decision::Feed,
                Decision::Feed,
                Decision::Feed
            ]
        );
        assert!(!gate.awaiting_keyframe());
    }

    /// While latched, P-frames alone never resume playback — only a keyframe
    /// can, because their references are gone.
    #[test]
    fn latch_holds_until_a_keyframe_arrives() {
        let mut gate = FrameGate::new(60);
        for _ in 0..BEHIND_DRAINS_BEFORE_SKIP {
            gate.decide(&p(32));
        }
        assert!(gate.awaiting_keyframe());

        for _ in 0..50 {
            assert!(gate.decide(&p(1)).iter().all(|d| *d == Decision::Skip));
            assert!(gate.awaiting_keyframe());
        }

        assert_eq!(gate.decide(&[true]), vec![Decision::Feed]);
        assert!(!gate.awaiting_keyframe());
    }

    /// The property that actually matters for "is the picture alive": over a
    /// realistic session with keyframes every 3s, the overwhelming majority of
    /// received frames must reach the decoder. The 2026-08-17 bug sat at 4%.
    #[test]
    fn realistic_session_feeds_almost_everything() {
        let mut gate = FrameGate::new(120);
        let gop = 360; // keyframe every 3s at 120fps
        for frame in 0..3_600u32 {
            // Mostly single-frame drains, with an occasional healthy burst.
            let batch = if frame % 97 == 0 { 40 } else { 1 };
            let mut flags = p(batch);
            if frame % gop == 0 {
                flags[0] = true;
            }
            gate.decide(&flags);
        }
        let total = gate.fed() + gate.skipped();
        let ratio = gate.fed() as f64 / total as f64;
        assert!(
            ratio > 0.95,
            "fed only {:.1}% of frames ({} fed / {} skipped)",
            ratio * 100.0,
            gate.fed(),
            gate.skipped()
        );
    }

    /// A low-fps stream keeps the floor, so it isn't *more* sensitive than the
    /// value proven on Android.
    #[test]
    fn threshold_floor_applies_to_slow_streams() {
        let mut gate = FrameGate::new(24);
        assert!(gate.decide(&p(10)).iter().all(|d| *d == Decision::Feed));
        assert_eq!(gate.skipped(), 0);
    }
}
