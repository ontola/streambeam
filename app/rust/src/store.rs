//! App-private storage paths (set from Flutter via [`crate::api::set_app_data_dir`]).

use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The device's own display name (e.g. the user-set Android device name), used
/// as our client name when watching and host name when beaming, so peers see a
/// recognizable name instead of a generic "streambeam-…". Set from Flutter.
static DEVICE_NAME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_data_dir(dir: &str) {
    let _ = DATA_DIR.set(PathBuf::from(dir));
}

pub fn set_device_name(name: &str) {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        *DEVICE_NAME.lock().unwrap() = Some(trimmed.to_string());
    }
}

/// The configured device name, if one has been set.
pub fn device_name() -> Option<String> {
    DEVICE_NAME.lock().unwrap().clone()
}

/// Our client name for outgoing connections — the device name, or a fallback.
pub fn client_name() -> String {
    device_name().unwrap_or_else(|| "StreamBeam".to_string())
}

pub fn data_dir() -> PathBuf {
    DATA_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(".streambeam-dev"))
}

pub fn client_identity_path() -> PathBuf {
    data_dir().join("client_identity.pkcs8")
}

pub fn saved_host_path() -> PathBuf {
    data_dir().join("saved_host.json")
}
