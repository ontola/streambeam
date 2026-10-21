//! Opus playback sink for the Watch role.
//!
//! Datagram loops hand raw Opus packets to [`AudioPlayback::push`]; a
//! dedicated thread owns the Opus decoder and feeds a shared PCM ring that
//! the platform output drains — AAudio's callback on Android, and on macOS an
//! `AVAudioSourceNode` in the Swift runner pulling through
//! `streambeam_macos_audio_fill` (the mirror image of the capture-side FFI).
//! Decode stays off the receive path, and playback failures disable audio for
//! the session but never affect video.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// Max decoded audio buffered ahead of the device (200 ms). Beyond this the
/// oldest samples are dropped, so latency snaps back after a network stall
/// instead of accumulating for the rest of the session.
const MAX_BUFFERED: usize = SAMPLE_RATE as usize / 5 * CHANNELS;
/// Largest Opus frame the decoder can emit (120 ms at 48 kHz, stereo).
const MAX_FRAME: usize = 5760 * CHANNELS;

type Ring = Arc<Mutex<VecDeque<i16>>>;

/// User "Sound" toggle from the player sidebar. Global so it survives the
/// automatic reconnects a quality change triggers; Dart re-syncs it whenever
/// a player screen mounts.
static MUTED: AtomicBool = AtomicBool::new(false);

pub fn set_muted(muted: bool) {
    MUTED.store(muted, Ordering::Relaxed);
}

pub struct AudioPlayback {
    tx: SyncSender<Vec<u8>>,
}

impl AudioPlayback {
    /// Start the playback thread. Always returns a handle: device or decoder
    /// failures are logged on the thread and packets are simply discarded.
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let _ = std::thread::Builder::new()
            .name("sb-audio-play".into())
            .spawn(move || playback_thread(rx));
        Self { tx }
    }

    /// Queue one raw Opus packet (datagram payload after the media header).
    /// Never blocks the receive loop: a full queue drops the packet.
    pub fn push(&self, opus: &[u8]) {
        let _ = self.tx.try_send(opus.to_vec());
    }
}

fn playback_thread(rx: Receiver<Vec<u8>>) {
    let mut decoder = match opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("audio playback disabled: opus decoder init failed: {e}");
            return;
        }
    };
    let ring: Ring = Arc::new(Mutex::new(VecDeque::new()));
    let _sink = match sink::start(ring.clone()) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("audio playback disabled: {e}");
            return;
        }
    };

    let mut pcm = vec![0i16; MAX_FRAME];
    // Ends when every AudioPlayback handle is dropped; the sink stops on drop.
    while let Ok(packet) = rx.recv() {
        if MUTED.load(Ordering::Relaxed) {
            // Drop instead of decoding; clear buffered audio so unmuting
            // doesn't replay a stale burst.
            ring.lock().unwrap().clear();
            continue;
        }
        match decoder.decode(&packet, &mut pcm, false) {
            Ok(frames) => {
                let mut ring = ring.lock().unwrap();
                ring.extend(&pcm[..frames * CHANNELS]);
                while ring.len() > MAX_BUFFERED {
                    ring.drain(..CHANNELS);
                }
            }
            Err(e) => log::debug!("opus decode failed: {e}"),
        }
    }
}

/// Fill `out` (interleaved stereo i16 frames at `device_rate`) from the
/// 48 kHz ring. Underruns play silence. A device that refused 48 kHz gets
/// nearest-frame resampling — crude, but that path is rare.
fn fill(ring: &Ring, out: &mut [i16], device_rate: u32, frac: &mut f64) {
    let mut ring = ring.lock().unwrap();
    if device_rate == SAMPLE_RATE {
        for sample in out.iter_mut() {
            *sample = ring.pop_front().unwrap_or(0);
        }
        return;
    }
    let step = f64::from(SAMPLE_RATE) / f64::from(device_rate.max(1));
    for frame in out.chunks_exact_mut(CHANNELS) {
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample = ring.get(i).copied().unwrap_or(0);
        }
        *frac += step;
        while *frac >= 1.0 {
            if ring.len() >= CHANNELS {
                ring.drain(..CHANNELS);
            }
            *frac -= 1.0;
        }
    }
}

/// Apple: the Swift runner owns an `AVAudioSourceNode` (started alongside the
/// video decoder) whose render callback pulls interleaved 48 kHz stereo f32
/// through [`fill_f32`]. Registering the ring here is all "starting" means.
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod sink {
    use std::sync::Mutex;

    use super::{Ring, SAMPLE_RATE, fill};

    static ACTIVE: Mutex<Option<Ring>> = Mutex::new(None);

    pub struct Sink;

    pub fn start(ring: Ring) -> Result<Sink, String> {
        *ACTIVE.lock().unwrap() = Some(ring);
        Ok(Sink)
    }

    impl Drop for Sink {
        fn drop(&mut self) {
            *ACTIVE.lock().unwrap() = None;
        }
    }

    /// Called from the Swift render callback; silence when no session plays.
    pub fn fill_f32(out: &mut [f32]) {
        let active = ACTIVE.lock().unwrap();
        let Some(ring) = active.as_ref() else {
            out.fill(0.0);
            return;
        };
        let mut scratch = vec![0i16; out.len()];
        // The source node is created at 48 kHz; AVAudioEngine resamples to
        // the hardware rate downstream, so no `frac` state is needed here.
        let mut frac = 0.0;
        fill(ring, &mut scratch, SAMPLE_RATE, &mut frac);
        for (dst, src) in out.iter_mut().zip(scratch) {
            *dst = f32::from(src) / f32::from(i16::MAX);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use sink::fill_f32;

/// Windows watch audio needs a WASAPI render sink — pending the next
/// Windows-box session (cpal was rejected: its coreaudio-sys/bindgen build
/// breaks the macOS app build this repo must keep working).
#[cfg(windows)]
mod sink {
    use super::Ring;

    pub struct Sink;

    pub fn start(_ring: Ring) -> Result<Sink, String> {
        Err("audio playback not yet implemented on Windows watch".into())
    }
}

/// Linux builds the experimental app shell; audio awaits a native backend.
#[cfg(target_os = "linux")]
mod sink {
    use super::Ring;

    pub struct Sink;

    pub fn start(_ring: Ring) -> Result<Sink, String> {
        Err("audio playback not yet implemented on Linux".into())
    }
}

#[cfg(target_os = "android")]
mod sink {
    use ndk::audio::{
        AudioCallbackResult, AudioFormat, AudioPerformanceMode, AudioStream, AudioStreamBuilder,
    };

    use super::{CHANNELS, Ring, SAMPLE_RATE, fill};

    pub struct Sink {
        stream: AudioStream,
    }

    pub fn start(ring: Ring) -> Result<Sink, String> {
        let mut frac = 0.0f64;
        let stream = AudioStreamBuilder::new()
            .map_err(|e| format!("AAudio builder: {e}"))?
            .sample_rate(SAMPLE_RATE as i32)
            .channel_count(CHANNELS as i32)
            .format(AudioFormat::PCM_I16)
            .performance_mode(AudioPerformanceMode::LowLatency)
            .data_callback(Box::new(move |stream, data, num_frames| {
                let out = unsafe {
                    std::slice::from_raw_parts_mut(
                        data.cast::<i16>(),
                        num_frames as usize * CHANNELS,
                    )
                };
                let rate = stream.sample_rate().max(1) as u32;
                fill(&ring, out, rate, &mut frac);
                AudioCallbackResult::Continue
            }))
            .open_stream()
            .map_err(|e| format!("AAudio open: {e}"))?;
        stream
            .request_start()
            .map_err(|e| format!("AAudio start: {e}"))?;
        Ok(Sink { stream })
    }

    impl Drop for Sink {
        fn drop(&mut self) {
            let _ = self.stream.request_stop();
        }
    }
}

/// Apple native audio callback. The legacy symbol name is retained so macOS
/// and iOS share the same decoder/audio implementation without an ABI change.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn streambeam_macos_audio_fill(out: *mut f32, len: usize) {
    if out.is_null() || len == 0 {
        return;
    }
    // SAFETY: AVAudioSourceNode owns this writable buffer for the duration
    // of its synchronous render callback.
    let buf = unsafe { std::slice::from_raw_parts_mut(out, len) };
    fill_f32(buf);
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn fill_drains_at_device_rate_and_zero_fills_underrun() {
        let ring: Ring = Arc::new(Mutex::new((1..=8i16).collect()));
        let mut out = [0i16; 12];
        let mut frac = 0.0;
        fill(&ring, &mut out, SAMPLE_RATE, &mut frac);
        assert_eq!(&out[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&out[8..], &[0, 0, 0, 0]);
        assert!(ring.lock().unwrap().is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn apple_sink_converts_stereo_pcm_and_silences_after_stop() {
        let ring: Ring = Arc::new(Mutex::new([i16::MAX, -i16::MAX].into_iter().collect()));
        let sink = sink::start(ring).unwrap();
        let mut out = [9.0; 4];
        fill_f32(&mut out);
        assert_eq!(out, [1.0, -1.0, 0.0, 0.0]);
        drop(sink);
        out.fill(9.0);
        fill_f32(&mut out);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn fill_resamples_when_device_rate_differs() {
        // 24 kHz device: every output frame consumes two 48 kHz frames.
        let ring: Ring = Arc::new(Mutex::new((1..=16i16).collect()));
        let mut out = [0i16; 4];
        let mut frac = 0.0;
        fill(&ring, &mut out, SAMPLE_RATE / 2, &mut frac);
        assert_eq!(out, [1, 2, 5, 6]);
    }
}
