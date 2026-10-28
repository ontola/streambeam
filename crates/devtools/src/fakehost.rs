//! Fake host: speaks the real protocol for the macOS dev loop.

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use streambeam_devtools::stream_ctl::StreamControl;
use streambeam_devtools::{audio_src, default_log_filter, media};
use streambeam_net::{
    HelloDecision, Identity, PairingStore, advertise, export_pairing_secret, pairing_addrs,
    peer_pubkey, read_msg, server_config, write_msg,
};
use streambeam_proto::PROTO_VERSION;
use streambeam_proto::control::{AudioParams, ClientMsg, HostInputMode, HostMsg};
use streambeam_proto::fec::{encode_audio_packet, encode_video_frame};
use streambeam_proto::pair::PairInfo;
use tokio::signal;

#[derive(Parser, Debug)]
#[command(name = "fakehost")]
struct Opt {
    /// Annex-B elementary stream (default: bundled `assets/test.h264`).
    #[arg(long)]
    file: Option<PathBuf>,

    /// Use synthetic frames instead of a bitstream file.
    #[arg(long)]
    synthetic: bool,

    /// Target frame rate.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Development-only private JSONL record of authenticated synthetic input.
    #[arg(long)]
    input_events_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(default_log_filter())
        .init();

    let opt = Opt::parse();
    if let Err(e) = run(opt).await {
        eprintln!("fakehost error: {e}");
        std::process::exit(1);
    }
}

fn load_catalog(
    opt: &Opt,
) -> Result<media::VideoCatalog, Box<dyn std::error::Error + Send + Sync>> {
    if opt.synthetic {
        Ok(media::VideoCatalog::synthetic(120))
    } else if let Some(path) = &opt.file {
        Ok(media::VideoCatalog::load(path)?)
    } else {
        Ok(media::VideoCatalog::bundled())
    }
}

async fn run(opt: Opt) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let input_log = if let Some(path) = &opt.input_events_file {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Some(Arc::new(Mutex::new(options.open(path)?)))
    } else {
        None
    };
    let catalog = Arc::new(load_catalog(&opt)?);
    let audio = Arc::new(audio_src::OpusCatalog::bundled());
    eprintln!(
        "loaded {} access units ({} keyframes), {} opus packets",
        catalog.len(),
        catalog.frames.iter().filter(|f| f.keyframe).count(),
        audio.len()
    );

    let listen: SocketAddr = std::env::var("STREAMBEAM_LISTEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:47989".parse().expect("addr"));

    let identity = Identity::generate()?;
    let store = Arc::new(Mutex::new(PairingStore::new()));
    let token = store.lock().unwrap().mint_token(300);
    let expires_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 300;

    let endpoint = quinn::Endpoint::server(server_config(&identity)?, listen)?;
    let addr = endpoint.local_addr()?;
    let addrs = pairing_addrs(addr);

    let _mdns = if std::env::var("STREAMBEAM_NO_MDNS").is_err() {
        let ips: Vec<_> = addrs.iter().map(|a| a.ip()).collect();
        match advertise(
            "fakehost",
            "fakehost",
            addr.port(),
            &ips,
            Some(identity.public_key()),
        ) {
            Ok(daemon) => {
                eprintln!("mDNS: broadcasting on port {} ({ips:?})", addr.port());
                Some(daemon)
            }
            Err(e) => {
                eprintln!("mDNS advertise skipped: {e}");
                None
            }
        }
    } else {
        None
    };

    let info = PairInfo {
        version: PROTO_VERSION,
        host_name: "fakehost".into(),
        host_pubkey: identity.public_key(),
        addrs,
        token,
        expires_unix,
    };

    // stdout stays the raw payload (scripts pipe it); the QR/link form is info.
    println!("{}", info.encode_payload()?);
    eprintln!("QR/link form: {}", info.encode_url()?);
    eprintln!("fakehost listening on {addr} — paste the sbm1: line into devclient");

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let store = store.clone();
                let catalog = catalog.clone();
                let audio = audio.clone();
                let fps = opt.fps;
                let input_log = input_log.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(incoming, store, catalog, audio, fps, input_log).await {
                        eprintln!("connection failed: {e}");
                    }
                });
            }
            _ = signal::ctrl_c() => {
                eprintln!("shutting down");
                break;
            }
        }
    }

    endpoint.wait_idle().await;
    Ok(())
}

async fn handle(
    incoming: quinn::Incoming,
    store: Arc<Mutex<PairingStore>>,
    catalog: Arc<media::VideoCatalog>,
    audio: Arc<audio_src::OpusCatalog>,
    fps: u32,
    input_log: Option<Arc<Mutex<std::fs::File>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = incoming.await?;
    eprintln!("client connected from {}", conn.remote_address());

    let (mut send, mut recv) = conn.accept_bi().await?;
    let mut buf = Vec::new();
    let hello: ClientMsg = read_msg(&mut recv, &mut buf).await?;
    let ClientMsg::Hello {
        proto_version,
        client_name,
        pair_proof,
    } = hello
    else {
        return Err("first message must be Hello".into());
    };

    if proto_version != PROTO_VERSION {
        write_msg(
            &mut send,
            &HostMsg::PairReject {
                reason: format!("unsupported proto version {proto_version}"),
            },
        )
        .await?;
        return Ok(());
    }

    let client_pk = peer_pubkey(&conn)?;
    let exporter = export_pairing_secret(&conn)?;
    let decision =
        store
            .lock()
            .unwrap()
            .handle_hello(client_pk, &client_name, pair_proof, &exporter);

    match decision {
        HelloDecision::AcceptedNewPair => {
            eprintln!("paired new client: {client_name}");
            write_msg(&mut send, &HostMsg::PairAck).await?;
        }
        HelloDecision::AcceptedKnown => {
            eprintln!("known client: {client_name}");
            write_msg(&mut send, &HostMsg::PairAck).await?;
        }
        HelloDecision::Rejected(reason) => {
            eprintln!("pair rejected for {client_name}: {reason}");
            write_msg(&mut send, &HostMsg::PairReject { reason }).await?;
            return Ok(());
        }
    }

    // Input is consumed only after Hello authorization; the fixture never injects it.
    let input_conn = conn.clone();
    tokio::spawn(async move {
        let Ok(mut recv) = input_conn.accept_uni().await else {
            return;
        };
        let mut buffer = Vec::new();
        while let Ok(event) =
            read_msg::<streambeam_proto::input::InputEvent>(&mut recv, &mut buffer).await
        {
            if let Some(log) = &input_log {
                let mut file = log.lock().unwrap();
                if serde_json::to_writer(&mut *file, &event).is_err() || writeln!(file).is_err() {
                    break;
                }
            }
        }
    });
    let stream_ctl = Arc::new(StreamControl::new(&catalog));
    let mut streaming = false;
    let frame_interval = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
    let mut last_idr = std::time::Instant::now();

    loop {
        let msg: ClientMsg =
            match tokio::time::timeout(Duration::from_secs(120), read_msg(&mut recv, &mut buf))
                .await
            {
                Ok(Ok(m)) => m,
                Ok(Err(_)) | Err(_) => break,
            };

        match msg {
            ClientMsg::SessionRequest { .. } => {
                // CodecConfig before SessionAccept so clients can prime decoders.
                if let Some(first) = catalog.frames.first() {
                    write_msg(
                        &mut send,
                        &HostMsg::CodecConfig {
                            data: first.data.clone(),
                        },
                    )
                    .await?;
                }
                write_msg(
                    &mut send,
                    &HostMsg::SessionAccept {
                        session_id: 1,
                        codec: "h264".into(),
                        width: 640,
                        height: 360,
                        fps: fps.min(255) as u8,
                        audio: AudioParams {
                            sample_rate: 48_000,
                            channels: 2,
                            frame_ms: 10,
                        },
                        input_mode: HostInputMode::Mouse,
                    },
                )
                .await?;
                eprintln!("session started — streaming video + audio");

                if !streaming {
                    streaming = true;
                    let media_conn = conn.clone();
                    let ctl = stream_ctl.clone();
                    let cat = catalog.clone();
                    tokio::spawn(async move {
                        if let Err(e) = stream_video(media_conn, cat, ctl, frame_interval).await {
                            eprintln!("video stream ended: {e}");
                        }
                    });
                    let audio_conn = conn.clone();
                    let audio_cat = audio.clone();
                    let audio_interval = Duration::from_millis(10);
                    tokio::spawn(async move {
                        if let Err(e) = stream_audio(audio_conn, audio_cat, audio_interval).await {
                            eprintln!("audio stream ended: {e}");
                        }
                    });
                }
            }
            ClientMsg::Ping { t_us } => {
                write_msg(&mut send, &HostMsg::Pong { t_us }).await?;
            }
            ClientMsg::RequestIdr => {
                if last_idr.elapsed() >= Duration::from_millis(250) {
                    stream_ctl.request_idr(&catalog);
                    last_idr = std::time::Instant::now();
                    eprintln!("IDR scheduled");
                }
            }
            ClientMsg::Bye => {
                write_msg(&mut send, &HostMsg::Bye).await?;
                break;
            }
            other => {
                eprintln!("ignored control message: {other:?}");
            }
        }
    }

    conn.close(0u32.into(), b"bye");
    Ok(())
}

async fn stream_video(
    conn: quinn::Connection,
    catalog: Arc<media::VideoCatalog>,
    ctl: Arc<StreamControl>,
    frame_interval: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let max_datagram = conn.max_datagram_size().unwrap_or(1200 + 16);
    let mut frame_id: u16 = 0;

    loop {
        if conn.close_reason().is_some() {
            break;
        }

        if ctl.take_force_idr() {
            // index already rewound to keyframe by request_idr
        }

        let idx = ctl.current_index();
        let au = &catalog.frames[idx];
        let shards = encode_video_frame(
            frame_id,
            &au.data,
            au.keyframe,
            media::capture_ts_us(),
            max_datagram,
        )?;

        for shard in shards {
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            if conn.send_datagram_wait(dg.into()).await.is_err() {
                return Ok(());
            }
        }

        if frame_id.is_multiple_of(30) {
            eprintln!(
                "sent frame {frame_id} catalog[{idx}] ({} bytes, keyframe={})",
                au.data.len(),
                au.keyframe
            );
        }

        ctl.advance();
        frame_id = frame_id.wrapping_add(1);
        tokio::time::sleep(frame_interval).await;
    }
    Ok(())
}

async fn stream_audio(
    conn: quinn::Connection,
    catalog: Arc<audio_src::OpusCatalog>,
    packet_interval: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut packet_id: u16 = 0;
    let mut idx = 0usize;

    loop {
        if conn.close_reason().is_some() {
            break;
        }

        let opus = &catalog.packets[idx % catalog.len()];
        let shard = encode_audio_packet(packet_id, opus, media::capture_ts_us());
        let mut dg = shard.header.encode().to_vec();
        dg.extend_from_slice(&shard.payload);
        if conn.send_datagram_wait(dg.into()).await.is_err() {
            return Ok(());
        }

        idx += 1;
        packet_id = packet_id.wrapping_add(1);
        tokio::time::sleep(packet_interval).await;
    }
    Ok(())
}

use std::sync::Mutex;
