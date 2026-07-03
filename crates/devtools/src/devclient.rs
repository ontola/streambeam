//! Headless dev client: pairing, FEC reassembly, hash verify, loss injection.

mod annex_b;
mod loss;
mod media;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use streambeam_core::FrameReassembler;
use streambeam_proto::control::{AudioCaps, ClientMsg, HostMsg, VideoCaps};
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};
use streambeam_proto::pair::PairInfo;
use streambeam_proto::PROTO_VERSION;
use streambeam_net::{
    build_pair_proof, client_config, export_pairing_secret, read_msg, write_msg, Identity,
};

#[derive(Parser, Debug)]
#[command(name = "devclient")]
struct Opt {
    /// Pairing payload from fakehost (`sbm1:...`).
    payload: String,

    /// Stop after this many complete video frames (0 = until Ctrl+C).
    #[arg(long, default_value_t = 120)]
    frames: u32,

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

    let info = PairInfo::decode_payload(&opt.payload)?;
    if info.version != PROTO_VERSION {
        return Err(format!("unsupported pair version {}", info.version).into());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now > info.expires_unix {
        return Err(
            "pairing payload expired — restart fakehost and use the new sbm1: line".into(),
        );
    }

    let addr = info
        .addrs
        .first()
        .copied()
        .ok_or("pairing payload has no addresses")?;

    let identity = Identity::generate()?;
    let mut client = quinn::Endpoint::client("[::]:0".parse()?)?;
    client.set_default_client_config(client_config(&identity, info.host_pubkey)?);

    println!("connecting to {} ({addr})…", info.host_name);
    let conn = client
        .connect(addr, "localhost")?
        .await
        .map_err(|e| format!("connect failed (is fakehost running?): {e}"))?;

    let exporter = export_pairing_secret(&conn)?;
    let pair_proof = Some(build_pair_proof(
        &info.token,
        &identity.public_key(),
        &exporter,
    ));

    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(
        &mut send,
        &ClientMsg::Hello {
            proto_version: PROTO_VERSION,
            client_name: "devclient".into(),
            pair_proof,
        },
    )
    .await?;

    let mut buf = Vec::new();
    match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
        HostMsg::PairAck => println!("paired ✓"),
        HostMsg::PairReject { reason } => {
            return Err(format!(
                "pair rejected: {reason} (restart fakehost for a fresh token)"
            )
            .into());
        }
        other => return Err(format!("unexpected host reply: {other:?}").into()),
    }

    write_msg(
        &mut send,
        &ClientMsg::SessionRequest {
            video: VideoCaps {
                h264: true,
                hevc: false,
                av1: false,
                max_width: 1920,
                max_height: 1080,
                max_fps: 60,
            },
            audio: AudioCaps {
                opus: true,
                max_channels: 2,
            },
            bitrate_kbps: 20_000,
        },
    )
    .await?;

    loop {
        match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
            HostMsg::SessionAccept {
                width,
                height,
                fps,
                codec,
                ..
            } => {
                println!("session started: {width}x{height} @ {fps} fps ({codec})");
                break;
            }
            HostMsg::CodecConfig { .. } => {}
            other => return Err(format!("expected SessionAccept, got {other:?}").into()),
        }
    }

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
    let mut last_report = std::time::Instant::now();
    let mut hash_failures = 0u64;
    let mut hash_ok = 0u64;
    let mut last_idr = std::time::Instant::now();

    'recv: loop {
        if target > 0 && asm.stats().frames_complete >= u64::from(target) {
            break;
        }

        let datagram = match tokio::time::timeout(Duration::from_secs(5), conn.read_datagram())
            .await
        {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => return Err(format!("datagram read failed: {e}").into()),
            Err(_) => {
                eprintln!("no datagrams for 5s — is fakehost still running?");
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
                            write_msg(&mut send, &ClientMsg::RequestIdr).await?;
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

        if asm.take_idr_request() && last_idr.elapsed() >= Duration::from_millis(250) {
            write_msg(&mut send, &ClientMsg::RequestIdr).await?;
            last_idr = std::time::Instant::now();
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

    let _ = write_msg(&mut send, &ClientMsg::Bye).await;
    conn.close(0u32.into(), b"bye");
    client.wait_idle().await;
    Ok(())
}
