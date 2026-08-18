//! App-private storage paths (set from Flutter via [`crate::api::set_app_data_dir`]).

use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The device's own display name (e.g. the user-set Android device name), used
/// as our client name when watching and host name when beaming, so peers see a
/// recognizable name instead of a generic "streambeam-…". Set from Flutter.
static DEVICE_NAME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Whether [`DEVICE_NAME`] came from the user rather than the platform. A
/// user's choice must survive the platform name being pushed in at every
/// startup, and survive restarts, so it is persisted and wins.
static NAME_IS_CUSTOM: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

const NAME_FILE: &str = "device_name.txt";

pub fn set_data_dir(dir: &str) {
    let _ = DATA_DIR.set(PathBuf::from(dir));
    // Adopt a previously chosen name as soon as we know where to look, so it
    // is in place before the platform pushes its own name in.
    if let Some(name) = std::fs::read_to_string(PathBuf::from(dir).join(NAME_FILE))
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
    {
        *DEVICE_NAME.lock().unwrap() = Some(name);
        *NAME_IS_CUSTOM.lock().unwrap() = true;
    }
}

/// Platform-provided name (the OS device name), pushed in at startup. Never
/// overrides a name the user chose.
pub fn set_device_name(name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() || *NAME_IS_CUSTOM.lock().unwrap() {
        return;
    }
    *DEVICE_NAME.lock().unwrap() = Some(trimmed.to_string());
}

/// User-chosen name: persisted, and takes precedence from here on.
pub fn set_custom_device_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("device name cannot be empty".into());
    }
    let dir = DATA_DIR.get().ok_or("app data dir not set yet")?;
    std::fs::write(dir.join(NAME_FILE), trimmed)
        .map_err(|e| format!("could not save the device name: {e}"))?;
    *DEVICE_NAME.lock().unwrap() = Some(trimmed.to_string());
    *NAME_IS_CUSTOM.lock().unwrap() = true;
    Ok(())
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
