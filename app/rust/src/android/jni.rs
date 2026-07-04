//! JNI helpers: a native window from a Java Surface (for playback), and the
//! screen-capture ingest path (MediaCodec output → host encoder) for hosting.

use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::{jboolean, jint};
use jni::JNIEnv;

/// Returns a raw `ANativeWindow*` suitable for [`ndk::native_window::NativeWindow::from_ptr`].
pub fn native_window_from_surface(env: JNIEnv, surface: JObject) -> i64 {
    let env_ptr = env.get_native_interface();
    let surface_ptr = surface.as_raw();
    let window = unsafe { ndk_sys::ANativeWindow_fromSurface(env_ptr, surface_ptr) };
    window as i64
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeWindowFromSurface<
    'local,
>(
    env: JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    surface: JObject<'local>,
) -> i64 {
    native_window_from_surface(env, surface)
}

// --- Screen-capture ingest (hosting from Android) ---
//
// The Kotlin ScreenCaptureService runs MediaProjection + a MediaCodec H.264
// encoder and pushes each output buffer here; the host server's
// `AndroidScreenEncoder` pulls them back out. See `streambeam_host::android_screen`.

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeBeginScreenSession<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    width: jint,
    height: jint,
) {
    streambeam_host::android_screen::begin_session(width.max(0) as u32, height.max(0) as u32);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeSubmitScreenFrame<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    data: JByteArray<'local>,
    keyframe: jboolean,
    is_config: jboolean,
    width: jint,
    height: jint,
) {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return;
    };
    streambeam_host::android_screen::submit(
        bytes,
        keyframe != 0,
        is_config != 0,
        width.max(0) as u32,
        height.max(0) as u32,
    );
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeEndScreenSession<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    streambeam_host::android_screen::end_session();
}

/// Kotlin polls this each encode tick; `true` means ask MediaCodec for a sync frame.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeTakeIdrRequest<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jboolean {
    jboolean::from(streambeam_host::android_screen::take_idr_request())
}
