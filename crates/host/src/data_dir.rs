//! Host data directory (%APPDATA%\\streambeam on Windows, ~/.streambeam elsewhere).

use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STREAMBEAM_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("streambeam");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".streambeam");
    }
    PathBuf::from(".streambeam-dev")
}

pub fn paired_store_path() -> PathBuf {
    data_dir().join("paired.json")
}

pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
