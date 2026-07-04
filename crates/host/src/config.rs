//! Shared host configuration for CLI and Flutter shell.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::data_dir::{ensure_data_dir, identity_path, paired_store_path};
use crate::server::{HostConfig, HostControls, PairApproval};

pub fn default_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "streambeam".into())
}

pub fn build_host_config(
    listen: SocketAddr,
    host_name: String,
    fps: u32,
    max_encode_dim: u32,
    token_ttl_secs: u64,
    pairing_payload_tx: Option<std::sync::mpsc::Sender<String>>,
    pairing_request_tx: Option<std::sync::mpsc::Sender<PairApproval>>,
    controls: Arc<HostControls>,
) -> HostConfig {
    let _ = ensure_data_dir();
    HostConfig {
        listen,
        host_name,
        fps,
        max_encode_dim,
        token_ttl_secs,
        identity_path: identity_path(),
        paired_store: Some(paired_store_path()),
        pairing_payload_tx,
        pairing_request_tx,
        controls,
    }
}
