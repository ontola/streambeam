//! Windows host pipeline traits. Real capture/encode lands in M2; mock works on macOS.

pub mod audio;
pub mod data_dir;
pub mod encode;
pub mod input_inject;
pub mod mock;
pub mod qr;
pub mod server;

pub use encode::{EncodeError, EncodedFrame, VideoEncoder};
