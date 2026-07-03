//! App-private storage paths (set from Flutter via [`crate::api::set_app_data_dir`]).

use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_dir(dir: &str) {
    let _ = DATA_DIR.set(PathBuf::from(dir));
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
