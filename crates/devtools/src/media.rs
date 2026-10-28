//! Video source: bundled Annex-B clip or synthetic fallback.

use std::path::Path;

use streambeam_core::annex_b::AccessUnit;

pub fn frame_digest(frame: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    frame.hash(&mut h);
    h.finish()
}

pub fn capture_ts_us() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u32
}

/// Deterministic fake frame (used with `--synthetic`).
pub fn synthetic_frame(frame_id: u16, keyframe: bool) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16_000);
    frame.extend_from_slice(b"SBFR");
    frame.extend_from_slice(&frame_id.to_le_bytes());
    frame.push(u8::from(keyframe));
    let seed = u64::from(frame_id).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut x = seed;
    while frame.len() < 16_000 {
        x = x.wrapping_shl(13) ^ x;
        x = x.wrapping_shr(7) ^ x;
        x = x.wrapping_shl(17) ^ x;
        frame.push(x as u8);
    }
    frame
}

#[derive(Debug, Clone)]
pub struct VideoCatalog {
    pub frames: Vec<AccessUnit>,
    pub digests: Vec<u64>,
}

impl VideoCatalog {
    pub fn from_annex_b(data: &[u8]) -> Self {
        let frames = streambeam_core::annex_b::split_access_units(data);
        let digests = frames.iter().map(|f| frame_digest(&f.data)).collect();
        Self { frames, digests }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read(path)?;
        Ok(Self::from_annex_b(&data))
    }

    pub fn bundled() -> Self {
        Self::from_annex_b(include_bytes!("../assets/test.h264"))
    }

    pub fn synthetic(count: usize) -> Self {
        let frames: Vec<AccessUnit> = (0..count)
            .map(|i| {
                let keyframe = i % 30 == 0;
                AccessUnit {
                    data: synthetic_frame(i as u16, keyframe),
                    keyframe,
                }
            })
            .collect();
        let digests = frames.iter().map(|f| frame_digest(&f.data)).collect();
        Self { frames, digests }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn previous_keyframe_index(&self, from: usize) -> usize {
        let n = self.frames.len();
        let from = from.min(n.saturating_sub(1));
        for i in (0..=from).rev() {
            if self.frames[i].keyframe {
                return i;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_clip_parses_sixty_access_units() {
        let data = include_bytes!("../assets/test.h264");
        assert_eq!(streambeam_core::annex_b::split_nal_units(data).len(), 65);
        let catalog = VideoCatalog::bundled();
        assert_eq!(catalog.len(), 60, "expected 2s @ 30fps");
        let idr = catalog.frames.iter().filter(|a| a.keyframe).count();
        assert_eq!(idr, 2, "GOP=30 → 2 IDR in 60 frames");
    }
}
