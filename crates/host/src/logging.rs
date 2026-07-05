//! Always-on file logger.
//!
//! Release builds run with `windows_subsystem = "windows"` (no console), so
//! `eprintln!` output is invisible — there is nowhere for it to go. This is the
//! only place diagnostics survive to be inspected after the fact.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data_dir::{ensure_data_dir, log_path};

static LOCK: Mutex<()> = Mutex::new(());

/// Print to stderr (visible in debug/console builds) and append to the log
/// file at `%APPDATA%\streambeam\host.log` (visible always).
pub fn log(msg: &str) {
    eprintln!("{msg}");
    let _guard = LOCK.lock().unwrap();
    let _ = ensure_data_dir();
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        // Millisecond precision: at a 30fps target each frame is ~33ms apart,
        // so whole-second timestamps are too coarse to see real frame pacing.
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}
