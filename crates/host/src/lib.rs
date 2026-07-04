//! Windows host pipeline traits. Real capture/encode lands in M2; mock works on macOS.

pub mod audio;
#[cfg(windows)]
pub mod capture;
pub mod config;
pub mod data_dir;
pub mod encode;
pub mod input_inject;
pub mod logging;
pub mod mock;
pub mod qr;
pub mod server;

pub use config::{build_host_config, default_host_name};
pub use encode::{EncodeError, EncodedFrame, VideoEncoder};
pub use server::{ClientEntry, HostConfig, HostControls, PairApproval};
