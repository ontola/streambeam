//! QUIC host server — pairs clients and streams video + audio.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streambeam_net::{
    advertise, advertise_ips, export_pairing_secret, peer_pubkey, read_msg, server_config,
    write_msg, HelloDecision, Identity, PairingStore,
};
use streambeam_proto::control::{AudioParams, ClientMsg, HostMsg};
use streambeam_proto::fec::{encode_audio_packet, encode_video_frame};
use streambeam_proto::pair::PairInfo;
use streambeam_proto::PROTO_VERSION;
use tokio::sync::Mutex as AsyncMutex;

use crate::audio::OpusCatalog;
use crate::encode::VideoEncoder;
use crate::mock::MockEncoder;

fn capture_ts_us() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u32
}

pub struct HostConfig {
    pub listen: SocketAddr,
    pub host_name: String,
    pub fps: u32,
    pub token_ttl_secs: u64,
    pub identity_path: std::path::PathBuf,
    pub paired_store: Option<std::path::PathBuf>,
}

struct StreamControl {
    force_idr: AtomicBool,
}

impl StreamControl {
    fn new() -> Self {
        Self {
            force_idr: AtomicBool::new(false),
        }
    }

    fn request_idr(&self) {
        self.force_idr.store(true, Ordering::Relaxed);
    }

    fn take_force_idr(&self) -> bool {
        self.force_idr.swap(false, Ordering::Relaxed)
    }
}

pub async fn run(config: HostConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let encoder = MockEncoder::bundled();
    let (width, height) = encoder.dimensions();
    let codec_config = encoder.codec_config();
    eprintln!(
        "encoder ready — {width}x{height}, {} frames, {} opus packets",
        encoder.frame_count(),
        OpusCatalog::bundled().len()
    );

    let identity = Identity::load_or_generate(&config.identity_path)?;
    let paired_path = config.paired_store.clone();
    let store = Arc::new(Mutex::new(
        paired_path
            .as_ref()
            .map(|p| PairingStore::load_from(p).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or_default(),
    ));
    if let Some(path) = &paired_path {
        eprintln!(
            "loaded {} paired client(s) from {}",
            store.lock().unwrap().paired_clients().len(),
            path.display()
        );
    }
    let token = store.lock().unwrap().mint_token(config.token_ttl_secs);
    let expires_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        + config.token_ttl_secs;

    let endpoint = quinn::Endpoint::server(server_config(&identity)?, config.listen)?;
    let addr = endpoint.local_addr()?;
    let addrs = pairing_addrs(addr);

    let _mdns = if std::env::var("STREAMBEAM_NO_MDNS").is_err() {
        let ips: Vec<_> = addrs.iter().map(|a| a.ip()).collect();
        match advertise(&config.host_name, &config.host_name, addr.port(), &ips) {
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

    let payload = PairInfo {
        version: 1,
        host_name: config.host_name.clone(),
        host_pubkey: identity.public_key(),
        addrs,
        token,
        expires_unix,
    }
    .encode_payload()?;

    println!("{payload}");
    crate::qr::print_terminal(&payload);
    eprintln!(
        "streambeam-host listening on {addr} — scan QR or paste sbm1: into the Android app"
    );
    eprintln!("LAN addresses: {:?}", pairing_addrs(addr));

    let audio = Arc::new(OpusCatalog::bundled());
    let frame_interval = Duration::from_micros(1_000_000 / u64::from(config.fps.max(1)));

    loop {
        let Some(incoming) = endpoint.accept().await else {
            break;
        };
        let store = store.clone();
        let audio = audio.clone();
        let codec_config = codec_config.clone();
        let paired_path = paired_path.clone();
        let fps = config.fps;
        let width = width as u16;
        let height = height as u16;

        tokio::spawn(async move {
            let encoder = MockEncoder::bundled();
            let encoder = Arc::new(AsyncMutex::new(encoder));
            if let Err(e) = handle_client(
                incoming,
                store,
                encoder,
                audio,
                codec_config,
                width,
                height,
                fps,
                frame_interval,
                paired_path,
            )
            .await
            {
                eprintln!("connection failed: {e}");
            }
        });
    }

    endpoint.wait_idle().await;
    Ok(())
}

fn pairing_addrs(listen: SocketAddr) -> Vec<SocketAddr> {
    let port = listen.port();
    if listen.ip().is_unspecified() {
        advertise_ips(listen.ip())
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect()
    } else {
        vec![listen]
    }
}

async fn handle_client(
    incoming: quinn::Incoming,
    store: Arc<Mutex<PairingStore>>,
    encoder: Arc<AsyncMutex<MockEncoder>>,
    audio: Arc<OpusCatalog>,
    codec_config: Option<Vec<u8>>,
    width: u16,
    height: u16,
    fps: u32,
    frame_interval: Duration,
    paired_store: Option<std::path::PathBuf>,
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
    let decision = store
        .lock()
        .unwrap()
        .handle_hello(client_pk, &client_name, pair_proof, &exporter);

    match decision {
        HelloDecision::AcceptedNewPair => {
            eprintln!("paired new client: {client_name}");
            write_msg(&mut send, &HostMsg::PairAck).await?;
            if let Some(path) = &paired_store {
                store
                    .lock()
                    .unwrap()
                    .save_to(path)
                    .map_err(|e| e.to_string())?;
            }
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

    let stream_ctl = Arc::new(StreamControl::new());
    let mut streaming = false;
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
                if let Some(data) = &codec_config {
                    write_msg(
                        &mut send,
                        &HostMsg::CodecConfig { data: data.clone() },
                    )
                    .await?;
                }
                write_msg(
                    &mut send,
                    &HostMsg::SessionAccept {
                        session_id: 1,
                        codec: "h264".into(),
                        width,
                        height,
                        fps: fps.min(255) as u8,
                        audio: AudioParams {
                            sample_rate: 48_000,
                            channels: 2,
                            frame_ms: 10,
                        },
                    },
                )
                .await?;
                eprintln!("session started — streaming to {client_name}");

                if !streaming {
                    streaming = true;
                    let media_conn = conn.clone();
                    let ctl = stream_ctl.clone();
                    let enc = encoder.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            stream_video(media_conn, enc, ctl, frame_interval).await
                        {
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
                    stream_ctl.request_idr();
                    {
                        let mut enc = encoder.lock().await;
                        enc.force_idr();
                    }
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
    encoder: Arc<AsyncMutex<MockEncoder>>,
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
            let mut enc = encoder.lock().await;
            enc.force_idr();
        }

        let frame = {
            let mut enc = encoder.lock().await;
            enc.encode_next()
                .map_err(|e| e.to_string())?
                .ok_or("encoder returned no frame")?
        };

        let shards = encode_video_frame(
            frame_id,
            &frame.data,
            frame.keyframe,
            capture_ts_us(),
            max_datagram,
        )?;

        for shard in shards {
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            if conn.send_datagram_wait(dg.into()).await.is_err() {
                return Ok(());
            }
        }

        if frame_id % 30 == 0 {
            eprintln!(
                "sent frame {frame_id} ({} bytes, keyframe={})",
                frame.data.len(),
                frame.keyframe
            );
        }

        frame_id = frame_id.wrapping_add(1);
        tokio::time::sleep(frame_interval).await;
    }
    Ok(())
}

async fn stream_audio(
    conn: quinn::Connection,
    catalog: Arc<OpusCatalog>,
    packet_interval: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut packet_id: u16 = 0;
    let mut idx = 0usize;

    loop {
        if conn.close_reason().is_some() {
            break;
        }

        let opus = &catalog.packets[idx % catalog.len()];
        let shard = encode_audio_packet(packet_id, opus, capture_ts_us());
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
