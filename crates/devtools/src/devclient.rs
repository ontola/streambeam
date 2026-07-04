//! Headless dev client: pairing, FEC reassembly, hash verify, loss injection.

mod annex_b;
mod loss;
mod media;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use streambeam_core::{
    connect_as_identity, connect_known, load_saved_host, save_saved_host, ClientSession,
    FrameReassembler, LossReport, SavedHost,
};
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};
use streambeam_proto::control::ClientMsg;
use streambeam_net::write_msg;

#[derive(Parser, Debug)]
#[command(name = "devclient")]
struct Opt {
    /// Pairing payload from fakehost (`sbm1:...`).
    payload: String,

    /// Stop after this many complete video frames (0 = until `--soak-secs` or Ctrl+C).
    #[arg(long, default_value_t = 120)]
    frames: u32,

    /// Run for this many seconds (overrides `--frames` when > 0).
    #[arg(long, default_value_t = 0)]
    soak_secs: u64,

    /// Randomly drop this percentage of incoming datagrams (0–100).
    #[arg(long, default_value_t = 0)]
    drop: u8,

    /// Reorder this percentage of datagrams (0–100).
    #[arg(long, default_value_t = 0)]
    reorder: u8,

    /// Verify each reassembled frame against the reference bitstream.
    #[arg(long, default_value_t = true)]
    verify: bool,

    /// Skip hash verification (pairing / soak tests).
    #[arg(long)]
    no_verify: bool,

    /// Annex-B reference file for hash verify (default: bundled clip).
    #[arg(long)]
    reference: Option<PathBuf>,

    /// Reconnect using saved host + persistent client identity (no QR token).
    #[arg(long)]
    reconnect: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(default_log_filter())
        .init();

    let opt = Opt::parse();
    let verify = opt.verify && !opt.no_verify;
    if let Err(e) = run(&opt, verify).await {
        eprintln!("devclient error: {e}");
        std::process::exit(1);
    }
}

fn default_log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))
}

fn load_reference(opt: &Opt) -> Result<media::VideoCatalog, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(path) = &opt.reference {
        Ok(media::VideoCatalog::load(path)?)
    } else {
        Ok(media::VideoCatalog::bundled())
    }
}

async fn run(opt: &Opt, verify: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let reference: Option<Arc<media::VideoCatalog>> = if verify {
        Some(Arc::new(load_reference(opt)?))
    } else {
        None
    };

    let mut session = open_session(opt).await?;
    println!(
        "session started: {}x{} @ {} fps ({})",
        session.width, session.height, session.fps, session.codec
    );

    if opt.drop > 0 || opt.reorder > 0 {
        println!("loss injection: drop {}%, reorder {}%", opt.drop, opt.reorder);
    }
    if verify {
        println!(
            "hash verify enabled ({} reference frames)",
            reference.as_ref().map(|c| c.len()).unwrap_or(0)
        );
    }
    println!(
        "receiving video… (--frames {} to auto-stop)",
        opt.frames
    );

    let mut asm = FrameReassembler::default();
    let mut loss = loss::LossInjector::new(opt.drop, opt.reorder);
    let target = opt.frames;
    let soak_deadline = (opt.soak_secs > 0).then(|| std::time::Instant::now() + Duration::from_secs(opt.soak_secs));
    let mut last_report = std::time::Instant::now();
    let mut hash_failures = 0u64;
    let mut hash_ok = 0u64;
    // Shared rate gate for both recovery messages this client can send
    // (RequestIdr on hash mismatch, RequestIdr/InvalidateRefFrames from the
    // reassembler's loss report) — one clock, so neither can spam the host
    // even if both fire the same tick.
    let mut last_idr = std::time::Instant::now();

    'recv: loop {
        if target > 0 && asm.stats().frames_complete >= u64::from(target) {
            break;
        }
        if soak_deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }

        let datagram = match tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram())
            .await
        {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => return Err(format!("datagram read failed: {e}").into()),
            Err(_) => {
                eprintln!("no datagrams for 5s — is host still running?");
                break 'recv;
            }
        };

        for datagram in loss.ingest(datagram.to_vec()) {
            if MediaHeader::decode(&datagram)?.channel == CHANNEL_AUDIO {
                continue;
            }
            if let Some((frame_id, frame)) = asm.push_datagram(&datagram)? {
                let digest = media::frame_digest(&frame);
                let n = asm.stats().frames_complete as usize;

                if let Some(catalog) = reference.as_ref() {
                    let catalog_idx = usize::from(frame_id) % catalog.len();
                    let expected = catalog.digests[catalog_idx];
                    if digest == expected {
                        hash_ok += 1;
                    } else {
                        hash_failures += 1;
                        eprintln!(
                            "hash mismatch frame_id {frame_id} catalog[{catalog_idx}] (got {digest:#018x}, expected {expected:#018x})"
                        );
                        if last_idr.elapsed() >= Duration::from_millis(250) {
                            write_msg(&mut session.send, &ClientMsg::RequestIdr).await?;
                            last_idr = std::time::Instant::now();
                        }
                    }
                }

                if n == 1 || n % 30 == 0 {
                    let tag = if reference.is_some() {
                        if hash_failures == 0 { "✓" } else { "!" }
                    } else {
                        " "
                    };
                    println!(
                        "frame #{n:>4} id {frame_id:>4}{tag}  {:>5} bytes  digest {digest:#018x}",
                        frame.len()
                    );
                }
            }
        }

        if let Some(report) = asm.take_loss_report() {
            if last_idr.elapsed() >= Duration::from_millis(250) {
                match report {
                    LossReport::Invalidate {
                        first_frame,
                        last_frame,
                    } => {
                        write_msg(
                            &mut session.send,
                            &ClientMsg::InvalidateRefFrames {
                                first_frame,
                                last_frame,
                            },
                        )
                        .await?;
                    }
                    LossReport::NeedIdr => {
                        write_msg(&mut session.send, &ClientMsg::RequestIdr).await?;
                    }
                }
                last_idr = std::time::Instant::now();
            }
        }

        if last_report.elapsed() >= Duration::from_secs(2) {
            let s = asm.stats();
            eprintln!(
                "stats: {} ok / {} complete / {} abandoned, {} shards received, dropped {} datagrams",
                hash_ok,
                s.frames_complete,
                s.frames_abandoned,
                s.shards_received,
                loss.datagrams_dropped
            );
            last_report = std::time::Instant::now();
        }
    }

    for datagram in loss.flush() {
        if MediaHeader::decode(&datagram)?.channel == CHANNEL_AUDIO {
            continue;
        }
        let _ = asm.push_datagram(&datagram)?;
    }

    let s = asm.stats();
    println!(
        "done — {} frames ({} hash ok, {} mismatch), {} shards, dropped {} / reordered {} datagrams",
        s.frames_complete,
        hash_ok,
        hash_failures,
        s.shards_received,
        loss.datagrams_dropped,
        loss.datagrams_reordered
    );

    if verify && hash_failures > 0 {
        return Err(format!("{hash_failures} frame(s) failed hash verify").into());
    }

    if soak_deadline.is_some() && asm.stats().frames_complete == 0 {
        return Err("soak received no complete frames".into());
    }

    let _ = write_msg(&mut session.send, &ClientMsg::Bye).await;
    session.conn.close(0u32.into(), b"bye");
    session._endpoint.wait_idle().await;
    Ok(())
}

fn client_data_dir() -> Option<PathBuf> {
    std::env::var("STREAMBEAM_CLIENT_DATA")
        .ok()
        .map(PathBuf::from)
}

fn client_identity_path(dir: &Path) -> PathBuf {
    dir.join("client_identity.pkcs8")
}

async fn open_session(opt: &Opt) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    if opt.reconnect {
        let dir = client_data_dir()
            .ok_or("set STREAMBEAM_CLIENT_DATA for --reconnect")?;
        let host = load_saved_host(&dir.join("saved_host.json"))?
            .ok_or("no saved host — pair once without --reconnect")?;
        let addr = host
            .primary_addr()
            .ok_or("saved host has no address")?;
        println!("reconnecting to {} ({addr})…", host.host_name);
        return connect_known(
            host.host_pubkey,
            addr,
            "devclient",
            &client_identity_path(&dir),
        )
        .await;
    }

    let id_path = client_data_dir().as_deref().map(client_identity_path);
    let session = connect_as_identity(
        &opt.payload,
        "devclient",
        id_path.as_deref(),
    )
    .await?;
    println!("paired ✓");

    if let Some(dir) = client_data_dir() {
        if let Ok(host) = SavedHost::from_pair_payload(&opt.payload) {
            save_saved_host(&dir.join("saved_host.json"), &host)?;
        }
    }

    Ok(session)
}
