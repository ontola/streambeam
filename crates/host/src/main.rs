//! LAN streaming host — Windows capture/encode plugs in behind [`VideoEncoder`] (M2).

use std::net::SocketAddr;

use clap::Parser;
use tokio::signal;

pub mod audio;
pub mod data_dir;
pub mod encode;
pub mod mock;
pub mod qr;
pub mod server;

pub use encode::{EncodedFrame, EncodeError, VideoEncoder};

use data_dir::{ensure_data_dir, identity_path, paired_store_path};

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

    /// Target frame rate (mock encoder loops bundled clip).
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// Pairing token lifetime in seconds.
    #[arg(long, default_value_t = 600)]
    token_ttl: u64,
}

fn default_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "streambeam".into())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let opt = Opt::parse();
    let listen = std::env::var("STREAMBEAM_LISTEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(opt.listen);
    let _ = ensure_data_dir();
    let config = server::HostConfig {
        listen,
        host_name: opt
            .name
            .or_else(|| std::env::var("STREAMBEAM_HOST_NAME").ok())
            .unwrap_or_else(default_host_name),
        fps: opt.fps,
        token_ttl_secs: opt.token_ttl,
        identity_path: identity_path(),
        paired_store: Some(paired_store_path()),
    };

    eprintln!(
        "StreamBeam host {} (mock encoder — real capture in M2)",
        env!("CARGO_PKG_VERSION")
    );

    tokio::select! {
        res = server::run(config) => {
            if let Err(e) = res {
                eprintln!("host error: {e}");
                std::process::exit(1);
            }
        }
        _ = signal::ctrl_c() => {
            eprintln!("shutting down");
        }
    }
}
