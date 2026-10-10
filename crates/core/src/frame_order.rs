//! Restore decode order after independent FEC groups complete out of order.
use std::collections::HashMap;
use std::time::Duration;

/// Wait briefly for a missing frame, without delaying an in-order stream.
/// A reported gap means the caller must recover its decoder reference chain.
pub struct FrameOrder<T> {
    expected: u16,
    pending: HashMap<u16, T>,
    blocked_since: Option<Duration>,
}

impl<T> Default for FrameOrder<T> {
    fn default() -> Self {
        Self {
            expected: 0,
            pending: HashMap::new(),
            blocked_since: None,
        }
    }
}

impl<T> FrameOrder<T> {
    pub fn push(&mut self, id: u16, frame: T) {
        // Late frames after a declared gap must never reach the decoder.
        if id.wrapping_sub(self.expected) as i16 >= 0 {
            self.pending.entry(id).or_insert(frame);
        }
    }

    /// Returns (gap before this frame, frame) in wrapping wire sequence order.
    pub fn drain(&mut self, now: Duration) -> Vec<(bool, T)> {
        let mut out = Vec::new();
        let mut gap = false;
        loop {
            if let Some(frame) = self.pending.remove(&self.expected) {
                out.push((gap, frame));
                gap = false;
                self.expected = self.expected.wrapping_add(1);
                self.blocked_since = None;
            } else if self.pending.is_empty() {
                self.blocked_since = None;
                break;
            } else {
                let since = *self.blocked_since.get_or_insert(now);
                if now.saturating_sub(since) < Duration::from_millis(30) && self.pending.len() < 32
                {
                    break;
                }
                self.expected = *self
                    .pending
                    .keys()
                    .min_by_key(|id| id.wrapping_sub(self.expected))
                    .unwrap();
                self.blocked_since = None;
                gap = true;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fec_completion_order_is_not_decode_order() {
        let mut q = FrameOrder::default();
        q.push(1, "inter");
        assert!(q.drain(Duration::ZERO).is_empty());
        q.push(0, "key");
        assert_eq!(
            q.drain(Duration::from_millis(8)),
            vec![(false, "key"), (false, "inter")]
        );
    }
    #[test]
    fn whole_frame_loss_is_bounded_and_late_frames_are_discarded() {
        let mut q = FrameOrder::default();
        q.push(0, 0);
        assert_eq!(q.drain(Duration::ZERO), vec![(false, 0)]);
        q.push(2, 2);
        assert!(q.drain(Duration::ZERO).is_empty());
        assert_eq!(q.drain(Duration::from_millis(30)), vec![(true, 2)]);
        q.push(1, 1);
        q.push(3, 3);
        assert_eq!(q.drain(Duration::from_millis(31)), vec![(false, 3)]);
    }
    #[test]
    fn sequence_wrap_does_not_create_a_gap() {
        let mut q = FrameOrder {
            expected: u16::MAX,
            ..FrameOrder::default()
        };
        q.push(0, 0);
        q.push(u16::MAX, 1);
        assert_eq!(q.drain(Duration::ZERO), vec![(false, 1), (false, 0)]);
    }
}
