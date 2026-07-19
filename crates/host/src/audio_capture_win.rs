// Win32/COM FFI glue — same conventions as capture.rs.
#![allow(unsafe_op_in_unsafe_fn)]

//! Windows system-audio capture: WASAPI **loopback** on the default render
//! endpoint ("what you hear"), feeding [`crate::audio::live::submit_pcm`] —
//! the Windows counterpart of macOS's ScreenCaptureKit audio tap. No virtual
//! audio device or driver: shared-mode loopback is the documented way to
//! record the mix a render endpoint is playing.
//!
//! Lifecycle: [`LoopbackCapture::start`] is a refcounted guard held by
//! `server::stream_audio` for each active session — the singleton capture
//! thread runs while at least one guard is alive, so audio is only being
//! captured while someone is actually watching (mirrors the macOS
//! capture-lease pattern), and a reconnect's brief guard overlap can't spawn
//! a second thread double-feeding the shared PCM queue.
//!
//! The endpoint's shared-mode mix format is whatever the user's device is
//! configured for — commonly float32 stereo at 44.1 or 48 kHz. `live` wants
//! interleaved 48 kHz stereo i16, so this module downmixes (first two
//! channels), converts, and linearly resamples when the rate differs. A
//! default-device change or format switch invalidates the client
//! (`AUDCLNT_E_DEVICE_INVALIDATED`); the thread just re-initializes against
//! the new default with a short backoff.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::core::GUID;

use crate::audio::{CHANNELS, SAMPLE_RATE};

/// Guards outstanding; the capture thread runs while > 0.
static GUARDS: AtomicUsize = AtomicUsize::new(0);
/// Management lock so start/stop can't race the thread's own exit: the
/// JoinHandle of the current (or just-exited) capture thread.
static THREAD: OnceLock<Mutex<Option<JoinHandle<()>>>> = OnceLock::new();

/// RAII lease on the system-audio capture thread.
pub struct LoopbackCapture(());

impl LoopbackCapture {
    pub fn start() -> Self {
        let slot = THREAD.get_or_init(|| Mutex::new(None));
        let mut thread = slot.lock().unwrap();
        GUARDS.fetch_add(1, Ordering::SeqCst);
        let running = thread.as_ref().is_some_and(|t| !t.is_finished());
        if !running {
            // Reap a finished handle before replacing it.
            if let Some(t) = thread.take() {
                let _ = t.join();
            }
            *thread = std::thread::Builder::new()
                .name("streambeam-audio-capture".into())
                .spawn(run)
                .ok();
        }
        Self(())
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        GUARDS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn run() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    crate::logging::log("audio capture: WASAPI loopback thread started");
    while GUARDS.load(Ordering::SeqCst) > 0 {
        match unsafe { capture_until_stopped() } {
            Ok(()) => {}
            Err(e) => {
                crate::logging::log(&format!(
                    "audio capture: {e} — retrying in 1s (device change/format switch?)"
                ));
                // Interruptible backoff so a stopping host doesn't linger.
                for _ in 0..10 {
                    if GUARDS.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
    crate::logging::log("audio capture: WASAPI loopback thread stopped");
}

/// Interleaved source layout accepted from the mix format.
enum SampleKind {
    F32,
    I16,
}

/// One WASAPI client session: init against the current default render
/// endpoint and pump until all guards drop or the device invalidates.
unsafe fn capture_until_stopped() -> Result<(), String> {
    let e = |what: &str, err: windows::core::Error| format!("{what}: {err}");

    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|err| e("CoCreateInstance MMDeviceEnumerator", err))?;
    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|err| e("GetDefaultAudioEndpoint", err))?;
    let client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|err| e("IMMDevice::Activate", err))?;

    let fmt_ptr = client.GetMixFormat().map_err(|err| e("GetMixFormat", err))?;
    // WAVEFORMATEX is repr(packed) in the windows crate — copy the fields
    // into locals once instead of referencing them in place (E0793).
    let fmt = std::ptr::read_unaligned(fmt_ptr);
    let src_rate = fmt.nSamplesPerSec;
    let src_channels = fmt.nChannels as usize;
    let bits = fmt.wBitsPerSample;
    let format_tag = fmt.wFormatTag;
    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    let kind = match (format_tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(SampleKind::F32),
        (WAVE_FORMAT_PCM, 16) => Some(SampleKind::I16),
        (WAVE_FORMAT_EXTENSIBLE, _) => {
            let ext = std::ptr::read_unaligned(fmt_ptr as *const WAVEFORMATEXTENSIBLE);
            const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
            const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
            let sub = ext.SubFormat;
            match (sub, bits) {
                (SUBTYPE_IEEE_FLOAT, 32) => Some(SampleKind::F32),
                (SUBTYPE_PCM, 16) => Some(SampleKind::I16),
                _ => None,
            }
        }
        _ => None,
    };
    // 100 ms shared buffer — loopback ignores periodicity.
    let init = client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK,
        1_000_000,
        0,
        fmt_ptr,
        None,
    );
    CoTaskMemFree(Some(fmt_ptr as *const _));
    init.map_err(|err| e("IAudioClient::Initialize(loopback)", err))?;
    let Some(kind) = kind else {
        return Err(format!(
            "unsupported mix format (tag {}, {} bits) — not float32/pcm16",
            format_tag, bits
        ));
    };
    if src_channels == 0 || src_rate == 0 {
        return Err("mix format reports zero channels or sample rate".into());
    }

    let capture: IAudioCaptureClient = client
        .GetService()
        .map_err(|err| e("GetService IAudioCaptureClient", err))?;
    client.Start().map_err(|err| e("IAudioClient::Start", err))?;
    crate::logging::log(&format!(
        "audio capture: loopback running ({src_rate} Hz, {src_channels} ch, {}) -> 48 kHz stereo",
        match kind {
            SampleKind::F32 => "f32",
            SampleKind::I16 => "i16",
        }
    ));

    let mut resampler = Resampler::new(src_rate);
    let mut stereo: Vec<i16> = Vec::with_capacity(4096);
    let result = (|| -> Result<(), String> {
        loop {
            if GUARDS.load(Ordering::SeqCst) == 0 {
                return Ok(());
            }
            // Event-driven wakeup isn't worth the plumbing here: 5 ms polling
            // against a 100 ms shared buffer can't underrun, and the thread
            // is idle-cheap.
            std::thread::sleep(std::time::Duration::from_millis(5));
            loop {
                let packet = capture
                    .GetNextPacketSize()
                    .map_err(|err| e("GetNextPacketSize", err))?;
                if packet == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|err| e("GetBuffer", err))?;
                let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                let n = frames as usize;
                stereo.clear();
                if silent {
                    stereo.resize(n * CHANNELS, 0);
                } else {
                    // Downmix: first two channels (or mono duplicated).
                    let right = (src_channels > 1) as usize;
                    match kind {
                        SampleKind::F32 => {
                            let s = std::slice::from_raw_parts(data as *const f32, n * src_channels);
                            for f in s.chunks_exact(src_channels) {
                                stereo.push(f32_to_i16(f[0]));
                                stereo.push(f32_to_i16(f[right]));
                            }
                        }
                        SampleKind::I16 => {
                            let s = std::slice::from_raw_parts(data as *const i16, n * src_channels);
                            for f in s.chunks_exact(src_channels) {
                                stereo.push(f[0]);
                                stereo.push(f[right]);
                            }
                        }
                    }
                }
                let _ = capture.ReleaseBuffer(frames);
                let out = resampler.push(&stereo);
                if !out.is_empty() {
                    crate::audio::live::submit_pcm(&out);
                }
            }
        }
    })();
    let _ = client.Stop();
    result
}

fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Linear resampler, interleaved stereo in/out. At 48 kHz input it's a pure
/// pass-through. Linear interpolation is audibly transparent for the
/// 44.1 -> 48 case this exists for (desktop/system audio, not mastering).
struct Resampler {
    src_rate: u32,
    /// Fractional read position into the (virtual, continuous) input stream,
    /// carried across calls.
    phase: f64,
    /// Last input frame of the previous call, for interpolation continuity.
    prev: [i16; CHANNELS],
    have_prev: bool,
}

impl Resampler {
    fn new(src_rate: u32) -> Self {
        Self {
            src_rate,
            phase: 0.0,
            prev: [0; CHANNELS],
            have_prev: false,
        }
    }

    fn push(&mut self, interleaved: &[i16]) -> Vec<i16> {
        if self.src_rate == SAMPLE_RATE {
            return interleaved.to_vec();
        }
        let in_frames = interleaved.len() / CHANNELS;
        if in_frames == 0 {
            return Vec::new();
        }
        let step = self.src_rate as f64 / SAMPLE_RATE as f64;
        // Prepend the carried frame so interpolation can cross call
        // boundaries without a discontinuity.
        let mut frames: Vec<[i16; CHANNELS]> = Vec::with_capacity(in_frames + 1);
        if self.have_prev {
            frames.push(self.prev);
        }
        for f in interleaved.chunks_exact(CHANNELS) {
            frames.push([f[0], f[1]]);
        }
        self.prev = *frames.last().unwrap();
        self.have_prev = true;

        let mut out = Vec::with_capacity((in_frames as f64 / step) as usize * CHANNELS + CHANNELS);
        while (self.phase.floor() as usize) + 1 < frames.len() {
            let idx = self.phase.floor() as usize;
            let frac = self.phase - idx as f64;
            for ch in 0..CHANNELS {
                let a = frames[idx][ch] as f64;
                let b = frames[idx + 1][ch] as f64;
                out.push((a + (b - a) * frac) as i16);
            }
            self.phase += step;
        }
        // Keep only the fractional remainder relative to the carried frame.
        self.phase -= (frames.len() - 1) as f64;
        self.phase = self.phase.max(0.0);
        out
    }
}
