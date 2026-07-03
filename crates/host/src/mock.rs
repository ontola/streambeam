//! Pre-encoded clip source — same asset as fakehost until M2 lands.

use streambeam_core::annex_b::split_access_units;

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder};

pub struct MockEncoder {
    frames: Vec<(Vec<u8>, bool)>,
    index: usize,
    width: u32,
    height: u32,
}

impl MockEncoder {
    pub fn bundled() -> Self {
        let data = include_bytes!("../../devtools/assets/test.h264");
        let frames = split_access_units(data);
        Self {
            frames: frames
                .into_iter()
                .map(|au| (au.data, au.keyframe))
                .collect(),
            index: 0,
            width: 640,
            height: 360,
        }
    }
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl VideoEncoder for MockEncoder {
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        if self.frames.is_empty() {
            return Ok(None);
        }
        let (data, keyframe) = self.frames[self.index].clone();
        self.index = (self.index + 1) % self.frames.len();
        Ok(Some(EncodedFrame {
            data,
            keyframe,
            width: self.width,
            height: self.height,
        }))
    }

    fn force_idr(&mut self) {
        for (i, (_, kf)) in self.frames.iter().enumerate() {
            if *kf {
                self.index = i;
                return;
            }
        }
        self.index = 0;
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// First keyframe access unit (SPS/PPS + IDR) for `CodecConfig`.
    fn codec_config(&self) -> Option<Vec<u8>> {
        self.frames
            .iter()
            .find(|(_, kf)| *kf)
            .map(|(data, _)| data.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_yields_sixty_frames() {
        let enc = MockEncoder::bundled();
        assert_eq!(enc.frame_count(), 60);
        let mut enc = enc;
        let first = enc.encode_next().unwrap().expect("frame");
        assert!(first.keyframe);
    }
}
