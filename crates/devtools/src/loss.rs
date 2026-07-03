//! Deterministic datagram loss / reorder injection for M1 soak tests.

use std::collections::VecDeque;

use rand::Rng;

#[derive(Debug, Default)]
pub struct LossInjector {
    drop_pct: u8,
    reorder_pct: u8,
    pub datagrams_seen: u64,
    pub datagrams_dropped: u64,
    pub datagrams_reordered: u64,
    jitter: VecDeque<Vec<u8>>,
    rng: rand::rngs::ThreadRng,
}

impl LossInjector {
    pub fn new(drop_pct: u8, reorder_pct: u8) -> Self {
        Self {
            drop_pct: drop_pct.min(100),
            reorder_pct: reorder_pct.min(100),
            rng: rand::rng(),
            ..Default::default()
        }
    }

    /// Returns datagrams to deliver (0..N) after applying drop/reorder.
    pub fn ingest(&mut self, datagram: Vec<u8>) -> Vec<Vec<u8>> {
        self.datagrams_seen += 1;
        if self.drop_pct > 0 && self.rng.random_ratio(self.drop_pct as u32, 100) {
            self.datagrams_dropped += 1;
            return Vec::new();
        }

        if self.reorder_pct > 0
            && !self.jitter.is_empty()
            && self.rng.random_ratio(self.reorder_pct as u32, 100)
        {
            self.datagrams_reordered += 1;
            let older = self.jitter.pop_front().unwrap();
            self.jitter.push_back(datagram);
            return vec![older];
        }

        if self.reorder_pct > 0 && self.rng.random_ratio(self.reorder_pct as u32, 100) {
            self.datagrams_reordered += 1;
            self.jitter.push_back(datagram);
            return Vec::new();
        }

        let mut out = Vec::with_capacity(1 + self.jitter.len());
        while let Some(d) = self.jitter.pop_front() {
            out.push(d);
        }
        out.push(datagram);
        out
    }

    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        self.jitter.drain(..).collect()
    }
}
