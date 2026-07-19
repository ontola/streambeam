//! Persist last-known host for reconnect without a fresh QR token.

use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedHost {
    pub host_name: String,
    pub host_pubkey: [u8; 32],
    pub addrs: Vec<SocketAddr>,
    /// Wake-on-LAN facts cached from the host's `HostMsg::WakeInfo` (v4) —
    /// present only after a successful session against a host that reports
    /// them. `#[serde(default)]` keeps pre-v4 saved_host.json files loading.
    #[serde(default)]
    pub mac: Option<[u8; 6]>,
    #[serde(default)]
    pub wol_broadcast: Option<SocketAddr>,
}

impl SavedHost {
    pub fn from_pair_payload(payload: &str) -> Result<Self, streambeam_proto::ProtoError> {
        let info = streambeam_proto::pair::PairInfo::decode_payload(payload)?;
        Ok(Self {
            host_name: info.host_name,
            host_pubkey: info.host_pubkey,
            addrs: info.addrs,
            mac: None,
            wol_broadcast: None,
        })
    }

    pub fn primary_addr(&self) -> Option<SocketAddr> {
        self.addrs.first().copied()
    }
}

pub fn load(path: &Path) -> Result<Option<SavedHost>, Box<dyn std::error::Error + Send + Sync>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(path)?;
    let host: SavedHost = serde_json::from_str(&data)?;
    Ok(Some(host))
}

pub fn save(path: &Path, host: &SavedHost) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(host)?;
    std::fs::write(path, data)?;
    Ok(())
}

pub fn clear(path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
