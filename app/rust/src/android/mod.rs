//! Android MediaCodec playback to a Surface.

mod jni;
pub mod play;

pub use jni::native_window_from_surface;
