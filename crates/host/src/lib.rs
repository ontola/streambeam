//! Windows host pipeline traits. Real capture/encode lands in M2; mock works on macOS.

pub mod encode;
pub mod mock;

pub use encode::{EncodedFrame, VideoEncoder};
