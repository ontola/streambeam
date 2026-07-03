//! Opus packet source for the fakehost dev loop.

use std::f32::consts::PI;

use opus::{Application, Channels, Encoder};

/// Pre-encoded Opus packets (10 ms, 48 kHz stereo).
#[derive(Debug, Clone)]
pub struct OpusCatalog {
    pub packets: Vec<Vec<u8>>,
}

impl OpusCatalog {
    /// Two-second 440 Hz tone — matches the bundled 60-frame / 30 fps clip.
    pub fn bundled() -> Self {
        Self::tone(2.0, 440.0)
    }

    pub fn tone(seconds: f32, hz: f32) -> Self {
        const SAMPLE_RATE: u32 = 48_000;
        const FRAME_MS: u32 = 10;
        const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

        let mut encoder =
            Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio).expect("opus encoder");
        let frame_count = (seconds * 1000.0 / FRAME_MS as f32).ceil() as usize;
        let mut packets = Vec::with_capacity(frame_count);
        let mut pcm = vec![0i16; SAMPLES_PER_FRAME * 2];
        let mut out = vec![0u8; 4000];

        for i in 0..frame_count {
            let t0 = i as f32 * FRAME_MS as f32 / 1000.0;
            for s in 0..SAMPLES_PER_FRAME {
                let t = t0 + s as f32 / SAMPLE_RATE as f32;
                let v = (2.0 * PI * hz * t).sin() * 0.25;
                let sample = (v * i16::MAX as f32) as i16;
                pcm[s * 2] = sample;
                pcm[s * 2 + 1] = sample;
            }
            let len = encoder.encode(&pcm, &mut out).expect("opus encode");
            packets.push(out[..len].to_vec());
        }

        Self { packets }
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_has_two_hundred_packets() {
        assert_eq!(OpusCatalog::bundled().len(), 200);
    }
}
