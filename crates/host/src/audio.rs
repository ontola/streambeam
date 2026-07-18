//! Opus audio sources for the host.
//!
//! Two sources live here, both behind the `audio` feature (on by default;
//! off for the Android host build, which is video-only in v1 and would
//! otherwise need the Opus C toolchain):
//!
//! - [`live`] — the real M5 path. The platform capture layer (ScreenCaptureKit
//!   on macOS, WASAPI on Windows later) pushes interleaved 48 kHz stereo i16
//!   PCM; this module encodes 10 ms Opus packets into a bounded queue that
//!   `server::stream_audio` drains. Mirrors the `macos_screen` submit-queue
//!   pattern used for video.
//! - [`OpusCatalog`] — the pre-encoded 440 Hz dev tone from M1, streamed only
//!   when a host explicitly opts in (`HostConfig::audio_tone_fallback`), so
//!   real sessions are silent rather than musical when no capture exists.

#[cfg(feature = "audio")]
use std::f32::consts::PI;

#[cfg(feature = "audio")]
use opus::{Application, Channels, Encoder};

/// Sample rate both sources encode at. Matches `AudioParams` in
/// `SessionAccept` and the M1 client decoders.
pub const SAMPLE_RATE: u32 = 48_000;
/// Interleaved channel count.
pub const CHANNELS: usize = 2;
/// Opus frame duration.
pub const FRAME_MS: usize = 10;
/// Per-channel samples in one Opus frame.
pub const SAMPLES_PER_FRAME: usize = SAMPLE_RATE as usize / 1000 * FRAME_MS;

/// Live PCM → Opus source fed by the platform capture layer.
#[cfg(feature = "audio")]
pub mod live {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{Application, CHANNELS, Channels, Encoder, SAMPLE_RATE, SAMPLES_PER_FRAME};

    const FRAME_LEN: usize = SAMPLES_PER_FRAME * CHANNELS;
    /// Encoded packets are dropped oldest-first past this bound (300 ms) so a
    /// stalled sender costs latency, never unbounded memory.
    const MAX_PACKETS: usize = 30;
    /// Pending PCM past this bound (~80 ms) is dropped oldest-first in whole
    /// frames, keeping frame alignment.
    const MAX_PENDING_PCM: usize = FRAME_LEN * 8;

    struct LiveState {
        encoder: Option<Encoder>,
        encoder_failed: bool,
        pcm: VecDeque<i16>,
        packets: VecDeque<Vec<u8>>,
        active: bool,
    }

    static STATE: Mutex<LiveState> = Mutex::new(LiveState {
        encoder: None,
        encoder_failed: false,
        pcm: VecDeque::new(),
        packets: VecDeque::new(),
        active: false,
    });

    /// Clear buffers and encoder state for a fresh capture session.
    pub fn reset() {
        let mut st = STATE.lock().unwrap();
        st.encoder = None;
        st.encoder_failed = false;
        st.pcm.clear();
        st.packets.clear();
        st.active = false;
    }

    /// True once PCM has arrived since the last [`reset`] — the signal for
    /// `stream_audio` that a real capture owns the audio channel.
    pub fn is_active() -> bool {
        STATE.lock().unwrap().active
    }

    /// Push interleaved 48 kHz stereo i16 PCM from the capture layer. Encodes
    /// every complete 10 ms frame into the packet queue.
    pub fn submit_pcm(samples: &[i16]) {
        let mut st = STATE.lock().unwrap();
        st.active = true;
        st.pcm.extend(samples);
        while st.pcm.len() > MAX_PENDING_PCM {
            st.pcm.drain(..FRAME_LEN);
        }

        if st.encoder.is_none() && !st.encoder_failed {
            match Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio) {
                Ok(enc) => st.encoder = Some(enc),
                Err(e) => {
                    st.encoder_failed = true;
                    crate::logging::log(&format!("live audio: opus encoder init failed: {e}"));
                }
            }
        }

        let mut frame = [0i16; FRAME_LEN];
        let mut out = [0u8; 1500];
        while st.pcm.len() >= FRAME_LEN {
            for s in frame.iter_mut() {
                *s = st.pcm.pop_front().unwrap();
            }
            let Some(enc) = st.encoder.as_mut() else {
                continue; // init failed: drain PCM so the buffer can't grow
            };
            match enc.encode(&frame, &mut out) {
                Ok(len) => {
                    if st.packets.len() >= MAX_PACKETS {
                        st.packets.pop_front();
                    }
                    st.packets.push_back(out[..len].to_vec());
                }
                Err(e) => {
                    crate::logging::log(&format!("live audio: opus encode failed: {e}"));
                }
            }
        }
    }

    /// Next encoded Opus packet, oldest first.
    pub fn take_packet() -> Option<Vec<u8>> {
        STATE.lock().unwrap().packets.pop_front()
    }
}

/// Stubs so callers compile in video-only builds.
#[cfg(not(feature = "audio"))]
pub mod live {
    pub fn reset() {}

    pub fn is_active() -> bool {
        false
    }

    pub fn submit_pcm(_samples: &[i16]) {}

    pub fn take_packet() -> Option<Vec<u8>> {
        None
    }
}

/// Pre-encoded Opus packets (10 ms, 48 kHz stereo).
#[derive(Debug, Clone)]
pub struct OpusCatalog {
    pub packets: Vec<Vec<u8>>,
}

impl OpusCatalog {
    /// Two-second 440 Hz tone — matches the bundled 60-frame / 30 fps clip.
    #[cfg(feature = "audio")]
    pub fn bundled() -> Self {
        Self::tone(2.0, 440.0)
    }

    /// Empty catalog when audio is disabled at compile time.
    #[cfg(not(feature = "audio"))]
    pub fn bundled() -> Self {
        Self {
            packets: Vec::new(),
        }
    }

    #[cfg(feature = "audio")]
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

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;

    #[test]
    fn live_pcm_encodes_into_bounded_packet_queue() {
        live::reset();
        assert!(!live::is_active());
        assert!(live::take_packet().is_none());

        // Half a frame: buffered, nothing encoded yet.
        live::submit_pcm(&vec![0i16; SAMPLES_PER_FRAME * CHANNELS / 2]);
        assert!(live::is_active());
        assert!(live::take_packet().is_none());

        // Completing the frame yields exactly one packet.
        live::submit_pcm(&vec![0i16; SAMPLES_PER_FRAME * CHANNELS / 2]);
        let packet = live::take_packet().expect("one encoded packet");
        assert!(!packet.is_empty());
        assert!(live::take_packet().is_none());

        // Far more PCM than the queue bound: drains to the packet cap, not
        // unbounded.
        live::submit_pcm(&vec![0i16; SAMPLES_PER_FRAME * CHANNELS * 100]);
        let mut drained = 0;
        while live::take_packet().is_some() {
            drained += 1;
        }
        assert!(drained <= 30, "queue must stay bounded, got {drained}");
        assert!(drained > 0);

        live::reset();
        assert!(!live::is_active());
        assert!(live::take_packet().is_none());
    }
}
