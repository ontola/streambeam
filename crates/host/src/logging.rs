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

/// Route panics into the same log file. Without this, a panic on a detached
/// background thread/task (capture, encode, or an NVENC session recreate)
/// unwinds silently in release builds — no console to print to — and the
/// video path just stops forever with nothing in `host.log` explaining why.
pub fn install_panic_hook() {
    // Opt-in diagnostics for desktop builds, where the Rust log facade may
    // otherwise have no sink. Keep transport packet tracing disabled.
    if std::env::var_os("STREAMBEAM_NETWORK_DIAGNOSTICS").is_some()
        && log::set_logger(&NETWORK_LOGGER).is_ok()
    {
        log::set_max_level(log::LevelFilter::Debug);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("PANIC: {info}"));
        default_hook(info);
    }));
}

struct NetworkLogger;
static NETWORK_LOGGER: NetworkLogger = NetworkLogger;

impl log::Log for NetworkLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("mdns_sd") || metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}
