//! Windows host pipeline traits. Real capture/encode lands in M2; mock works on macOS.

mod active_stream;
#[cfg(any(target_os = "android", test))]
pub mod android_input;
#[cfg(target_os = "android")]
pub mod android_screen;
pub mod audio;
#[cfg(windows)]
pub mod capture;
pub mod config;
pub mod data_dir;
pub mod encode;
pub mod gamepad_inject;
pub mod input_inject;
pub mod logging;
#[cfg(target_os = "macos")]
pub mod macos_screen;
pub mod mock;
#[cfg(windows)]
pub mod nvenc;
pub mod qr;
#[cfg(windows)]
pub mod scale;
pub mod server;
#[cfg(windows)]
pub mod viiper_server;

pub use config::{build_host_config, default_host_name};
pub use encode::{EncodeError, EncodedFrame, VideoEncoder};
pub use server::{ClientEntry, HostConfig, HostControls, PairApproval};
