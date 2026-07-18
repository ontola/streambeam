//! Direct native bridge for the macOS ScreenCaptureKit/VideoToolbox runner.
//!
//! These symbols are called from Swift without routing encoded frame bytes
//! through Dart or a Flutter method channel.

use std::{slice, str};

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_begin(
    width: u32,
    height: u32,
    display_id: u32,
    origin_x: f64,
    origin_y: f64,
    display_width: f64,
    display_height: f64,
) {
    streambeam_host::macos_screen::begin_session(
        width,
        height,
        display_id,
        origin_x,
        origin_y,
        display_width,
        display_height,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_requested_display() -> u32 {
    streambeam_host::macos_screen::requested_display().unwrap_or(u32::MAX)
}

/// Longest encoded side selected by the watching client's stream profile.
#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_requested_max_dimension() -> u32 {
    streambeam_host::macos_screen::requested_max_dimension()
}

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_request_revision() -> u32 {
    streambeam_host::macos_screen::capture_request_revision()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn streambeam_macos_capture_set_displays(
    json: *const u8,
    len: usize,
) {
    if json.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { slice::from_raw_parts(json, len) };
    let Ok(text) = str::from_utf8(bytes) else { return };
    let Ok(displays) = serde_json::from_str(text) else { return };
    streambeam_host::macos_screen::set_displays(displays);
}

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_end() {
    streambeam_host::macos_screen::end_session();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn streambeam_macos_capture_submit(
    bytes: *const u8,
    len: usize,
    keyframe: u8,
    is_config: u8,
) {
    if bytes.is_null() || len == 0 {
        return;
    }
    // SAFETY: Swift keeps the Data buffer alive for this synchronous call;
    // Rust copies it into an owned Vec before returning.
    let data = unsafe { slice::from_raw_parts(bytes, len) }.to_vec();
    streambeam_host::macos_screen::submit(data, keyframe != 0, is_config != 0);
}

/// Interleaved 48 kHz stereo i16 PCM from ScreenCaptureKit system audio.
/// `len` is the total sample count (frames × 2 channels).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn streambeam_macos_capture_submit_audio(samples: *const i16, len: usize) {
    if samples.is_null() || len == 0 {
        return;
    }
    // SAFETY: Swift keeps the buffer alive for this synchronous call; Rust
    // copies the samples into its own ring before returning.
    let pcm = unsafe { slice::from_raw_parts(samples, len) };
    streambeam_host::audio::live::submit_pcm(pcm);
}

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_take_idr_request() -> u8 {
    u8::from(streambeam_host::macos_screen::take_idr_request())
}

/// Polled by the native runner. True only while an authorized client is
/// requesting a stream; capture permission and capture lifetime stay native.
#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_wanted() -> u8 {
    u8::from(streambeam_host::macos_screen::capture_wanted())
}

/// Render-callback pull for the Watch role: fills `out` with interleaved
/// 48 kHz stereo f32 from the active playback session, or silence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn streambeam_macos_audio_fill(out: *mut f32, len: usize) {
    if out.is_null() || len == 0 {
        return;
    }
    // SAFETY: Swift's AVAudioSourceNode owns the buffer for the duration of
    // this synchronous render callback.
    let buf = unsafe { slice::from_raw_parts_mut(out, len) };
    crate::audio_play::fill_f32(buf);
}
