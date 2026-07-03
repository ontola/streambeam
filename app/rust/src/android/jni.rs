//! JNI helpers to obtain [`ndk::native_window::NativeWindow`] from a Java Surface.

use jni::objects::JObject;
use jni::JNIEnv;

/// Returns a raw `ANativeWindow*` suitable for [`ndk::native_window::NativeWindow::from_ptr`].
pub fn native_window_from_surface(mut env: JNIEnv, surface: JObject) -> i64 {
    let env_ptr = env.get_native_interface();
    let surface_ptr = surface.as_raw();
    let window = unsafe { ndk_sys::ANativeWindow_fromSurface(env_ptr, surface_ptr) };
    window as i64
}

#[no_mangle]
pub extern "system" fn Java_io_streambeam_streambeam_NativeStream_nativeWindowFromSurface<
    'local,
>(
    env: JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    surface: JObject<'local>,
) -> i64 {
    native_window_from_surface(env, surface)
}
