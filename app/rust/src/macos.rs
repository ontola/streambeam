//! Direct native bridge for the macOS ScreenCaptureKit/VideoToolbox runner.
//!
//! These symbols are called from Swift without routing encoded frame bytes
//! through Dart or a Flutter method channel.

use std::slice;

#[unsafe(no_mangle)]
pub extern "C" fn streambeam_macos_capture_begin(width: u32, height: u32) {
    streambeam_host::macos_screen::begin_session(width, height);
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
