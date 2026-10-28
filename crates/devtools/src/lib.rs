//! Shared building blocks for the devtools binaries (`fakehost`, `devclient`,
//! `playclient`, ...). Living in one library crate instead of per-binary
//! `mod` declarations keeps each module compiled once and lets rustc see
//! which items are actually dead.

pub mod audio_src;
pub mod loss;
pub mod media;
pub mod play;
pub mod stream_ctl;

/// `RUST_LOG`, defaulting to `warn` so binaries whose stdout/stderr is parsed
/// by scripts and tests stay quiet.
pub fn default_log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))
}
