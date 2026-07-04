// Release builds are a GUI app (no console window); debug keeps the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! LAN streaming host — Windows capture/encode plugs in behind [`VideoEncoder`] (M2).

use std::net::SocketAddr;

use clap::Parser;

pub mod audio;
#[cfg(windows)]
mod capture;
pub mod config;
pub mod data_dir;
pub mod encode;
pub mod input_inject;
pub mod logging;
pub mod mock;
pub mod qr;
pub mod server;
#[cfg(windows)]
mod tray;

pub use config::{build_host_config, default_host_name};
pub use encode::{EncodedFrame, EncodeError, VideoEncoder};
pub use server::{ClientEntry, HostConfig, HostControls, PairApproval};

#[derive(Parser, Debug)]
#[command(
    name = "streambeam-host",
    about = "StreamBeam host — pair via QR, stream H.264 + Opus to Android"
)]
struct Opt {
    /// Listen address (use 0.0.0.0 for LAN / Android). Override with STREAMBEAM_LISTEN.
    #[arg(long, default_value = "0.0.0.0:47989")]
    listen: SocketAddr,

    /// Host name shown in pairing QR and mDNS. Override with STREAMBEAM_HOST_NAME.
    #[arg(long)]
    name: Option<String>,

    /// Target frame rate (mock encoder loops bundled clip). Override with STREAMBEAM_FPS.
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// Cap the encoded video's longest side to this many pixels — desktop
    /// capture stays at native resolution, but a keyframe at native 4K can
    /// exceed the FEC frame-sharding limit, and a phone screen has no use for
    /// more detail than this anyway. Override with STREAMBEAM_MAX_ENCODE_DIM.
    #[arg(long, default_value_t = 1920)]
    max_encode_dim: u32,

    /// Pairing token lifetime in seconds.
    #[arg(long, default_value_t = 600)]
    token_ttl: u64,

    /// Run without the legacy Win32 tray window (default on Windows).
    #[arg(long)]
    headless: bool,

    /// Legacy Win32 tray + QR window (deprecated — use the Flutter desktop app).
    #[cfg(windows)]
    #[arg(long)]
    legacy_tray: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let opt = Opt::parse();

    #[cfg(windows)]
    {
        if opt.legacy_tray {
            if let Err(e) = tray::run(opt) {
                eprintln!("host error: {e}");
                std::process::exit(1);
            }
            return;
        }
        if let Err(e) = run_cli(opt) {
            eprintln!("host error: {e}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(not(windows))]
    {
        if let Err(e) = run_cli(opt) {
            eprintln!("host error: {e}");
            std::process::exit(1);
        }
    }
}

fn build_config(
    opt: Opt,
    pairing_payload_tx: Option<std::sync::mpsc::Sender<String>>,
    pairing_request_tx: Option<std::sync::mpsc::Sender<PairApproval>>,
    controls: std::sync::Arc<server::HostControls>,
) -> server::HostConfig {
    let listen = std::env::var("STREAMBEAM_LISTEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(opt.listen);
    let fps = std::env::var("STREAMBEAM_FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(opt.fps);
    let max_encode_dim = std::env::var("STREAMBEAM_MAX_ENCODE_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(opt.max_encode_dim);
    build_host_config(
        listen,
        opt.name
            .or_else(|| std::env::var("STREAMBEAM_HOST_NAME").ok())
            .unwrap_or_else(default_host_name),
        fps,
        max_encode_dim,
        opt.token_ttl,
        pairing_payload_tx,
        pairing_request_tx,
        controls,
    )
}

fn run_cli(opt: Opt) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let controls = std::sync::Arc::new(server::HostControls::new());
    let config = build_config(opt, None, None, controls);
    eprintln!(
        "StreamBeam host {} (mock encoder — real capture in M2)",
        env!("CARGO_PKG_VERSION")
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        tokio::select! {
            res = server::run(config) => res,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("shutting down");
                Ok(())
            }
        }
    })
}
