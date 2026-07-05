//! QUIC host server — pairs clients and streams video + audio.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streambeam_net::{
    Advertiser, HelloVerdict, Identity, PairingStore, advertise_ips, export_pairing_secret,
    peer_pubkey, read_msg, server_config, write_msg,
};
use streambeam_proto::PROTO_VERSION;
use streambeam_proto::control::{AudioParams, ClientMsg, HostMsg};
use streambeam_proto::fec::{encode_audio_packet, encode_video_frame};
use streambeam_proto::input::InputEvent;
use streambeam_proto::pair::PairInfo;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::OnceCell;
use tokio::sync::oneshot;

use crate::active_stream::{ActiveStreamTracker, CloseableConn};
use crate::audio::OpusCatalog;
use crate::encode::VideoEncoder;
use crate::mock::MockEncoder;

impl CloseableConn for quinn::Connection {
    fn close_stream(&self) {
        self.close(0u32.into(), b"superseded by new session");
    }
}

/// `eprintln!`-alike that also survives into `host.log` — plain `eprintln!`
/// is invisible once the release build has no console attached.
macro_rules! log {
    ($($arg:tt)*) => {
        crate::logging::log(&format!($($arg)*))
    };
}

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
    /// Cap the encoded video's longest side to this many pixels (see
    /// `build_encoder`/`DesktopEncoder::new` for why this exists).
    pub max_encode_dim: u32,
    pub token_ttl_secs: u64,
    pub identity_path: std::path::PathBuf,
    pub paired_store: Option<std::path::PathBuf>,
    /// Pushes the `sbm1:` pairing payload to the UI so it can render the QR.
    pub pairing_payload_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Asks the UI to approve each new device before it is enrolled. When
    /// `None` (headless host) new devices with a valid token are auto-approved.
    pub pairing_request_tx: Option<std::sync::mpsc::Sender<PairApproval>>,
    /// Live toggles + connected-client list shared with the UI.
    pub controls: Arc<HostControls>,
}

/// Runtime state the tray GUI reads and flips: discoverability, whether new
/// connections are accepted, and the list of currently connected clients.
pub struct HostControls {
    discoverable: AtomicBool,
    accept_connections: AtomicBool,
    clients: Mutex<Vec<ClientEntry>>,
    /// The connection currently driving `stream_video`/`stream_audio`, if
    /// any. The shared encoder (Desktop Duplication is single-instance) only
    /// supports one real viewer: without this, a stale connection that never
    /// got a clean QUIC close (app backgrounded, force-closed, network drop)
    /// keeps its `stream_video` loop alive forever, silently competing with
    /// any later connection for the same `encode_next()` calls — each
    /// connection only gets a fraction of the frames, which looks exactly
    /// like "0 fps, black screen" to whichever client loses the race. See
    /// `active_stream.rs` for the handoff logic and its regression test.
    active_stream: ActiveStreamTracker<quinn::Connection>,
    /// Set once by `run()` so `forget` can also drop the persisted pairing.
    pairing: Mutex<Option<(Arc<Mutex<PairingStore>>, Option<std::path::PathBuf>)>>,
    /// Set by `request_shutdown` to stop `run`'s accept loop; the `Notify`
    /// wakes it if it's currently parked on `endpoint.accept()`.
    shutdown_requested: AtomicBool,
    shutdown: tokio::sync::Notify,
}

#[derive(Clone)]
pub struct ClientEntry {
    pub name: String,
    pub addr: SocketAddr,
    pub pubkey: [u8; 32],
    conn: quinn::Connection,
}

impl Default for HostControls {
    fn default() -> Self {
        Self::new()
    }
}

impl HostControls {
    /// Both toggles start enabled.
    pub fn new() -> Self {
        Self {
            discoverable: AtomicBool::new(true),
            accept_connections: AtomicBool::new(true),
            clients: Mutex::new(Vec::new()),
            active_stream: ActiveStreamTracker::new(|prev_addr, addr| {
                log!("closing stale stream from {prev_addr} — new session from {addr}");
            }),
            pairing: Mutex::new(None),
            shutdown_requested: AtomicBool::new(false),
            shutdown: tokio::sync::Notify::new(),
        }
    }

    /// Ask `run` to stop accepting connections and return, releasing the port.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        self.shutdown.notify_waiters();
    }

    fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }

    /// Wire in the pairing store so `forget` can drop the persisted pairing,
    /// not just close the live connection. Called once from `run()`.
    fn attach_pairing_store(
        &self,
        store: Arc<Mutex<PairingStore>>,
        paired_path: Option<std::path::PathBuf>,
    ) {
        *self.pairing.lock().unwrap() = Some((store, paired_path));
    }

    /// Claim the shared encoder for `(addr, conn)` — see `active_stream.rs`
    /// for the handoff logic (and why it must *wait*, not just close).
    async fn start_streaming(&self, addr: SocketAddr, conn: &quinn::Connection) {
        self.active_stream.start_streaming(addr, conn).await;
    }

    /// Record the `stream_video` task handle for `addr` once it's spawned
    /// (can't be known at `start_streaming` time, before the task exists).
    fn set_stream_task(&self, addr: SocketAddr, task: tokio::task::JoinHandle<()>) {
        self.active_stream.set_stream_task(addr, task);
    }

    /// Release the shared encoder if `addr` is still the connection holding it.
    fn clear_active_stream(&self, addr: SocketAddr) {
        self.active_stream.clear_active_stream(addr);
    }

    /// True if `addr` is still the connection holding the shared encoder.
    fn is_active_stream(&self, addr: SocketAddr) -> bool {
        self.active_stream.is_active_stream(addr)
    }

    /// Close a connected client's connection. It stays paired and can
    /// reconnect without a new approval.
    pub fn disconnect(&self, addr: SocketAddr) -> bool {
        let clients = self.clients.lock().unwrap();
        let Some(entry) = clients.iter().find(|c| c.addr == addr) else {
            return false;
        };
        entry.conn.close(0u32.into(), b"disconnected by host");
        true
    }

    /// Close the connection *and* remove the pairing, so the device needs a
    /// fresh approval to reconnect.
    pub fn forget(&self, addr: SocketAddr) -> bool {
        let pubkey = {
            let clients = self.clients.lock().unwrap();
            let Some(entry) = clients.iter().find(|c| c.addr == addr) else {
                return false;
            };
            entry.conn.close(0u32.into(), b"removed by host");
            entry.pubkey
        };
        if let Some((store, path)) = &*self.pairing.lock().unwrap() {
            let mut store = store.lock().unwrap();
            store.forget(&pubkey);
            if let Some(path) = path {
                if let Err(e) = store.save_to(path) {
                    log!("failed to persist forgotten client: {e}");
                }
            }
        }
        true
    }

    pub fn is_discoverable(&self) -> bool {
        self.discoverable.load(Ordering::Relaxed)
    }

    pub fn set_discoverable(&self, on: bool) {
        self.discoverable.store(on, Ordering::Relaxed);
    }

    pub fn accepts_connections(&self) -> bool {
        self.accept_connections.load(Ordering::Relaxed)
    }

    pub fn set_accepts_connections(&self, on: bool) {
        self.accept_connections.store(on, Ordering::Relaxed);
    }

    /// Snapshot of currently connected clients for display.
    pub fn clients(&self) -> Vec<ClientEntry> {
        self.clients.lock().unwrap().clone()
    }

    fn add_client(&self, entry: ClientEntry) {
        let mut clients = self.clients.lock().unwrap();
        // Same device reconnecting gets a new ephemeral UDP port — keep one row per name.
        clients.retain(|c| c.name != entry.name);
        clients.push(entry);
    }

    fn remove_client(&self, addr: SocketAddr) {
        self.clients.lock().unwrap().retain(|c| c.addr != addr);
    }
}

/// Removes a client from the shared list when the connection handler exits.
struct ClientGuard {
    controls: Arc<HostControls>,
    addr: SocketAddr,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.controls.remove_client(self.addr);
        self.controls.clear_active_stream(self.addr);
    }
}

/// A pending "device wants to link" prompt handed to the UI thread.
///
/// The UI shows the prompt and reports the user's choice by sending `true`
/// (allow) or `false` (deny) on [`respond`](Self::respond). Dropping the
/// sender counts as a denial.
pub struct PairApproval {
    pub client_name: String,
    pub client_addr: SocketAddr,
    pub respond: oneshot::Sender<bool>,
}

struct StreamControl {
    force_idr: AtomicBool,
    /// Pending RFI invalidate range, `(first_frame, last_frame)`. A newer
    /// report simply replaces the previous one rather than widening it: by
    /// the time a second `InvalidateRefFrames` arrives, the client's own view
    /// of "what's lost" already supersedes the first report (its own
    /// `LossReport` already folds prior losses into one contiguous run), so
    /// coalescing to "newest wins" is simpler than merging ranges and no less
    /// correct here.
    invalidate: Mutex<Option<(u16, u16)>>,
}

impl StreamControl {
    fn new() -> Self {
        Self {
            force_idr: AtomicBool::new(false),
            invalidate: Mutex::new(None),
        }
    }

    fn request_idr(&self) {
        self.force_idr.store(true, Ordering::Relaxed);
    }

    fn take_force_idr(&self) -> bool {
        self.force_idr.swap(false, Ordering::Relaxed)
    }

    fn request_invalidate(&self, first_frame: u16, last_frame: u16) {
        *self.invalidate.lock().unwrap() = Some((first_frame, last_frame));
    }

    fn take_invalidate(&self) -> Option<(u16, u16)> {
        self.invalidate.lock().unwrap().take()
    }
}

/// Build the video source: real desktop capture on Windows, else the test clip.
fn build_encoder(fps: u32, max_encode_dim: u32) -> Box<dyn VideoEncoder> {
    #[cfg(windows)]
    {
        match crate::capture::DesktopEncoder::new(fps, max_encode_dim) {
            Ok(enc) => {
                let (w, h) = enc.dimensions();
                crate::logging::log(&format!("desktop capture ready — {w}x{h} @ {fps}fps"));
                return Box::new(enc);
            }
            Err(e) => {
                crate::logging::log(&format!(
                    "desktop capture unavailable ({e}) — using bundled test clip"
                ));
            }
        }
    }
    #[cfg(target_os = "android")]
    {
        // On Android the host is only ever started as part of beaming, so the
        // capture service (MediaProjection + MediaCodec) is on its way — but it
        // starts asynchronously and may not have delivered the codec config yet
        // when we get here. Wait for it before falling back to the mock clip.
        for _ in 0..50 {
            if crate::android_screen::is_ready() {
                crate::logging::log("android screen capture ready — streaming live screen");
                return Box::new(crate::android_screen::AndroidScreenEncoder);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        crate::logging::log("android screen capture didn't become ready — using mock clip");
    }
    let _ = (fps, max_encode_dim);
    Box::new(MockEncoder::bundled())
}

pub async fn run(config: HostConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(windows)]
    let _viiper = crate::viiper_server::ViiperGuard::ensure();

    // One shared encoder feeds every connection (Desktop Duplication is
    // single-instance, so multiple simultaneous viewers aren't supported yet).
    // Built lazily on the first real connection, not here: DXGI Desktop
    // Duplication + the NVENC session are expensive, persistent GPU
    // resources, and used to be created unconditionally at host startup —
    // held open for the whole process lifetime even with zero viewers ever
    // connecting. That's wasteful and was a suspected contributor to
    // GPU/audio hiccups observed with no active stream at all.
    let encoder_cell: Arc<
        OnceCell<(Arc<AsyncMutex<Box<dyn VideoEncoder>>>, u16, u16, Option<Vec<u8>>)>,
    > = Arc::new(OnceCell::new());

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
        log!(
            "loaded {} paired client(s) from {}",
            store.lock().unwrap().paired_clients().len(),
            path.display()
        );
    }
    let token = store.lock().unwrap().mint_token(config.token_ttl_secs);
    let expires_unix =
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + config.token_ttl_secs;

    let endpoint = quinn::Endpoint::server(server_config(&identity)?, config.listen)?;
    let addr = endpoint.local_addr()?;
    let addrs = pairing_addrs(addr);

    let controls = config.controls.clone();
    controls.attach_pairing_store(store.clone(), paired_path.clone());

    let _mdns = if std::env::var("STREAMBEAM_NO_MDNS").is_err() {
        let ips: Vec<_> = addrs
            .iter()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_loopback())
            .collect();
        let ips: Vec<_> = if ips.is_empty() {
            addrs.iter().map(|a| a.ip()).collect()
        } else {
            ips
        };
        match Advertiser::new(
            &config.host_name,
            &config.host_name,
            addr.port(),
            &ips,
            Some(identity.public_key()),
        ) {
            Ok(adv) => {
                let adv = Arc::new(adv);
                if let Err(e) = adv.set_visible(controls.is_discoverable()) {
                    log!("mDNS advertise skipped: {e}");
                }
                log!("mDNS: ready on port {} ({ips:?})", addr.port());
                // Poll the tray toggle and register/unregister to match it.
                let watch_adv = adv.clone();
                let watch_controls = controls.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        let want = watch_controls.is_discoverable();
                        if want != watch_adv.is_visible() {
                            match watch_adv.set_visible(want) {
                                Ok(()) => log!("mDNS discoverable = {want}"),
                                Err(e) => log!("mDNS toggle failed: {e}"),
                            }
                        }
                    }
                });
                Some(adv)
            }
            Err(e) => {
                log!("mDNS advertise skipped: {e}");
                None
            }
        }
    } else {
        None
    };

    let info = PairInfo {
        version: 1,
        host_name: config.host_name.clone(),
        host_pubkey: identity.public_key(),
        addrs,
        token,
        expires_unix,
    };
    let payload = info.encode_payload()?;
    // The QR carries the streambeam.eu link form: it opens the app directly
    // when installed, or the download/fallback page when not. The raw sbm1:
    // line stays on stdout for piping into devclient/playclient.
    let link = info.encode_url()?;

    println!("{payload}");
    crate::qr::print_terminal(&link);
    if let Some(tx) = &config.pairing_payload_tx {
        let _ = tx.send(link);
    }
    log!("streambeam-host listening on {addr} — scan QR or paste sbm1: into the Android app");
    log!("LAN addresses: {:?}", pairing_addrs(addr));

    let audio = Arc::new(OpusCatalog::bundled());
    let frame_interval = Duration::from_micros(1_000_000 / u64::from(config.fps.max(1)));

    loop {
        if controls.is_shutdown_requested() {
            break;
        }
        let incoming = tokio::select! {
            biased;
            _ = controls.shutdown.notified() => break,
            inc = endpoint.accept() => inc,
        };
        let Some(incoming) = incoming else {
            break;
        };
        if !controls.accepts_connections() {
            log!(
                "refused {} — connections disabled in tray",
                incoming.remote_address()
            );
            incoming.refuse();
            continue;
        }
        let (encoder, width, height, codec_config) = encoder_cell
            .get_or_init(|| async {
                let encoder = build_encoder(config.fps, config.max_encode_dim);
                let (width, height) = encoder.dimensions();
                let codec_config = encoder.codec_config();
                log!(
                    "encoder ready — {width}x{height}, {} opus packets",
                    OpusCatalog::bundled().len()
                );
                (
                    Arc::new(AsyncMutex::new(encoder)),
                    width as u16,
                    height as u16,
                    codec_config,
                )
            })
            .await
            .clone();

        let store = store.clone();
        let audio = audio.clone();
        let paired_path = paired_path.clone();
        let approval_tx = config.pairing_request_tx.clone();
        let controls = controls.clone();
        let fps = config.fps;

        tokio::spawn(async move {
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
                approval_tx,
                controls,
            )
            .await
            {
                log!("connection failed: {e}");
            }
        });
    }

    // Close any live connections immediately and release the UDP port so a
    // restart (Stop → Start beaming) can bind it again.
    endpoint.close(0u32.into(), b"host stopped");
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
    encoder: Arc<AsyncMutex<Box<dyn VideoEncoder>>>,
    audio: Arc<OpusCatalog>,
    codec_config: Option<Vec<u8>>,
    width: u16,
    height: u16,
    fps: u32,
    frame_interval: Duration,
    paired_store: Option<std::path::PathBuf>,
    approval_tx: Option<std::sync::mpsc::Sender<PairApproval>>,
    controls: Arc<HostControls>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = incoming.await?;
    log!("client connected from {}", conn.remote_address());

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
    // Evaluate under the lock, but never hold it across the user-approval await.
    let verdict = store
        .lock()
        .unwrap()
        .evaluate_hello(client_pk, pair_proof, &exporter);

    match verdict {
        HelloVerdict::AcceptedKnown => {
            log!("known client: {client_name}");
            write_msg(&mut send, &HostMsg::PairAck).await?;
        }
        HelloVerdict::Rejected(reason) => {
            log!("pair rejected for {client_name}: {reason}");
            write_msg(&mut send, &HostMsg::PairReject { reason }).await?;
            return Ok(());
        }
        HelloVerdict::NeedsApproval { proof_verified } => {
            if !proof_verified && approval_tx.is_none() {
                // Headless host, no approval UI, and the client presented no
                // pairing proof: refuse rather than auto-enroll. Auto-approving
                // this case would let any LAN device self-enroll (the original
                // vulnerability this branch closes) — a proof-less Hello may
                // only be enrolled via an explicit interactive approval.
                log!(
                    "pair rejected for {client_name}: no pairing proof and no interactive approval UI"
                );
                write_msg(
                    &mut send,
                    &HostMsg::PairReject {
                        reason: "pairing requires a valid proof or interactive approval".into(),
                    },
                )
                .await?;
                return Ok(());
            }
            let approved =
                request_approval(approval_tx.as_ref(), &client_name, conn.remote_address()).await;
            if !approved {
                log!("user declined pairing for {client_name}");
                write_msg(
                    &mut send,
                    &HostMsg::PairReject {
                        reason: "pairing declined on host".into(),
                    },
                )
                .await?;
                return Ok(());
            }

            store
                .lock()
                .unwrap()
                .commit_new_pair(client_pk, &client_name);
            log!("paired new client: {client_name}");
            write_msg(&mut send, &HostMsg::PairAck).await?;
            if let Some(path) = &paired_store {
                store
                    .lock()
                    .unwrap()
                    .save_to(path)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    // Accepted — track it in the shared list until this handler returns.
    controls.add_client(ClientEntry {
        name: client_name.clone(),
        addr: conn.remote_address(),
        pubkey: client_pk,
        conn: conn.clone(),
    });
    let _client_guard = ClientGuard {
        controls: controls.clone(),
        addr: conn.remote_address(),
    };

    let input_conn = conn.clone();
    tokio::spawn(async move {
        loop {
            match input_conn.accept_uni().await {
                Ok(recv) => {
                    log!("input stream opened");
                    if let Err(e) = handle_input_stream(recv).await {
                        log!("input stream ended: {e}");
                    }
                }
                Err(_) => break,
            }
        }
    });

    let stream_ctl = Arc::new(StreamControl::new());
    let mut streaming = false;
    let mut last_idr = std::time::Instant::now();
    // RFI's invalidate is cheaper to service than a full IDR (no keyframe
    // burst), so it gets a looser gate than RequestIdr's 250ms — still capped
    // so a misbehaving/hostile client can't spam the control stream.
    let mut last_invalidate = std::time::Instant::now();

    loop {
        let msg: ClientMsg = tokio::select! {
            _ = conn.closed() => break,
            read = read_msg(&mut recv, &mut buf) => match read {
                Ok(m) => m,
                Err(_) => break,
            },
        };

        match msg {
            // `video.{max_width,max_height,max_fps}` is where a client could
            // request its own resolution/fps cap, but the shared encoder is
            // built once at host startup (`build_encoder`, before any session
            // exists) — honoring a per-client request means deferring that
            // build to the first SessionRequest instead. Not wired up yet.
            ClientMsg::SessionRequest { .. } => {
                // Don't rely solely on the value captured at host startup
                // (`codec_config`, passed in) — some encoders don't have it
                // available immediately: hardware MFTs need a real keyframe
                // produced first (see capture.rs), and on Android the
                // MediaCodec CSD (SPS/PPS) can arrive shortly after the host
                // starts. Start streaming *first* (below) so `stream_video`'s
                // own naturally-paced, timeout-protected `encode_next()` loop
                // is what actually drives the encoder — then just wait
                // (asynchronously, not blocking a worker thread) for
                // `codec_config` to show up as a side effect. Driving
                // `encode_next()` synchronously here ourselves was tried and
                // reverted: it can't tell "screen hasn't changed yet" from
                // "encoder stalled", and blocked the whole connection for
                // 50+ seconds in practice on this machine's hardware
                // encoder — worse than the bug it was meant to fix.
                if !streaming {
                    streaming = true;
                    controls.start_streaming(conn.remote_address(), &conn).await;
                    stream_ctl.request_idr();
                    let media_conn = conn.clone();
                    let ctl = stream_ctl.clone();
                    let enc = encoder.clone();
                    let media_controls = controls.clone();
                    let media_addr = conn.remote_address();
                    let video_task = tokio::spawn(async move {
                        match stream_video(
                            media_conn,
                            enc,
                            ctl,
                            frame_interval,
                            media_controls,
                            media_addr,
                        )
                        .await
                        {
                            Ok(()) => log!("video stream ended: connection closed"),
                            Err(e) => log!("video stream ended: {e}"),
                        }
                    });
                    controls.set_stream_task(conn.remote_address(), video_task);
                    let audio_conn = conn.clone();
                    let audio_cat = audio.clone();
                    let audio_interval = Duration::from_millis(10);
                    tokio::spawn(async move {
                        if let Err(e) = stream_audio(audio_conn, audio_cat, audio_interval).await {
                            log!("audio stream ended: {e}");
                        }
                    });
                }

                let mut fresh_config = encoder.lock().await.codec_config();
                if fresh_config.is_none() {
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while fresh_config.is_none() && std::time::Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        fresh_config = encoder.lock().await.codec_config();
                    }
                    if fresh_config.is_none() {
                        log!(
                            "codec_config still unknown after 5s — sending SessionAccept without CodecConfig"
                        );
                    }
                }
                if let Some(data) = fresh_config.as_ref().or(codec_config.as_ref()) {
                    write_msg(&mut send, &HostMsg::CodecConfig { data: data.clone() }).await?;
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
                log!("session started — streaming to {client_name}");
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
                    log!("IDR scheduled");
                }
            }
            ClientMsg::InvalidateRefFrames {
                first_frame,
                last_frame,
            } => {
                if last_invalidate.elapsed() >= Duration::from_millis(60) {
                    stream_ctl.request_invalidate(first_frame, last_frame);
                    last_invalidate = std::time::Instant::now();
                    log!("invalidate ref frames scheduled: [{first_frame}, {last_frame}]");
                }
            }
            ClientMsg::Bye => {
                write_msg(&mut send, &HostMsg::Bye).await?;
                break;
            }
            other => {
                log!("ignored control message: {other:?}");
            }
        }
    }

    conn.close(0u32.into(), b"bye");
    Ok(())
}

async fn handle_input_stream(
    mut recv: quinn::RecvStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = Vec::new();
    let result = async {
        loop {
            let event: InputEvent = read_msg(&mut recv, &mut buf).await?;
            crate::input_inject::apply(event);
        }
    }
    .await;
    #[cfg(windows)]
    crate::gamepad_inject::reset();
    result
}

/// Ask the UI to approve a new device and wait for the answer.
///
/// With no UI wired (headless host) a valid token is trusted and auto-approved.
/// If the UI thread hangs up without answering, the request is denied.
async fn request_approval(
    approval_tx: Option<&std::sync::mpsc::Sender<PairApproval>>,
    client_name: &str,
    client_addr: SocketAddr,
) -> bool {
    let Some(tx) = approval_tx else {
        return true;
    };
    let (respond, answer) = oneshot::channel();
    if tx
        .send(PairApproval {
            client_name: client_name.to_owned(),
            client_addr,
            respond,
        })
        .is_err()
    {
        // UI thread is gone — fail closed.
        return false;
    }
    log!("awaiting host approval for {client_name} ({client_addr})");
    answer.await.unwrap_or(false)
}

async fn stream_video(
    conn: quinn::Connection,
    encoder: Arc<AsyncMutex<Box<dyn VideoEncoder>>>,
    ctl: Arc<StreamControl>,
    frame_interval: Duration,
    controls: Arc<HostControls>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let max_datagram = conn
        .max_datagram_size()
        .unwrap_or(1200 + streambeam_proto::media::MEDIA_HEADER_LEN);
    let mut frame_id: u16 = 0;
    // Rolling per-report-window timing, to find where per-frame latency goes:
    // waiting on the encoder (capture+encode+channel round trip), FEC sharding,
    // or actually pushing shards onto the wire.
    let mut encode_wait_us: u128 = 0;
    let mut shard_us: u128 = 0;
    let mut send_us: u128 = 0;
    let mut window_start = std::time::Instant::now();

    loop {
        if let Some(reason) = conn.close_reason() {
            log!("stream_video: connection closed ({reason})");
            break;
        }
        // See `HostControls::is_active_stream` — an immediate, in-memory
        // check that doesn't wait for the QUIC close handshake above, so a
        // superseded session can't sneak in even one more `encode_next()`
        // call and steal a frame meant for whichever connection replaced it.
        if !controls.is_active_stream(addr) {
            log!("stream_video: superseded by a newer session, stopping");
            break;
        }

        // Pace to frame_interval from the START of this iteration, not on top
        // of however long encode/shard/send already took — sleeping a full
        // frame_interval unconditionally after a slow (e.g. 90ms) encoder wait
        // was adding another 33ms on top of that every single frame, capping
        // throughput well under what the encoder could otherwise sustain.
        let iter_start = std::time::Instant::now();

        // Drain both signals every iteration so neither goes stale, but only
        // act on one: a pending IDR is the stronger recovery and makes any
        // pending invalidate moot (the keyframe it forces doesn't reference
        // anything the invalidate range was trying to avoid referencing).
        let force_idr = ctl.take_force_idr();
        let invalidate = ctl.take_invalidate();
        if force_idr {
            let mut enc = encoder.lock().await;
            enc.force_idr();
        } else if let Some((first, last)) = invalidate {
            let mut enc = encoder.lock().await;
            enc.invalidate_ref_frames(first, last);
        }

        let t_encode = std::time::Instant::now();
        let frame = {
            let mut enc = encoder.lock().await;
            enc.encode_next().map_err(|e| e.to_string())?
        };
        encode_wait_us += t_encode.elapsed().as_micros();
        // No frame this tick (e.g. screen unchanged) — wait and retry.
        let Some(frame) = frame else {
            tokio::time::sleep(frame_interval.saturating_sub(iter_start.elapsed())).await;
            continue;
        };

        let t_shard = std::time::Instant::now();
        let shards = match encode_video_frame(
            frame_id,
            &frame.data,
            frame.keyframe,
            capture_ts_us(),
            max_datagram,
        ) {
            Ok(shards) => shards,
            Err(e) => {
                log!(
                    "dropping frame {frame_id} ({} bytes): FEC encode failed: {e}",
                    frame.data.len()
                );
                frame_id = frame_id.wrapping_add(1);
                tokio::time::sleep(frame_interval.saturating_sub(iter_start.elapsed())).await;
                continue;
            }
        };
        shard_us += t_shard.elapsed().as_micros();

        let t_send = std::time::Instant::now();
        for shard in shards {
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            if conn.send_datagram_wait(dg.into()).await.is_err() {
                return Ok(());
            }
        }
        send_us += t_send.elapsed().as_micros();

        if frame_id % 30 == 0 {
            let window_secs = window_start.elapsed().as_secs_f64().max(0.001);
            log!(
                "sent frame {frame_id} ({} bytes, keyframe={}) — last 30 frames: {:.1} fps, avg encoder wait {} ms, FEC shard {} ms, send {} ms",
                frame.data.len(),
                frame.keyframe,
                30.0 / window_secs,
                encode_wait_us / 30 / 1000,
                shard_us / 30 / 1000,
                send_us / 30 / 1000,
            );
            encode_wait_us = 0;
            shard_us = 0;
            send_us = 0;
            window_start = std::time::Instant::now();
        }

        frame_id = frame_id.wrapping_add(1);
        tokio::time::sleep(frame_interval.saturating_sub(iter_start.elapsed())).await;
    }
    Ok(())
}

async fn stream_audio(
    conn: quinn::Connection,
    catalog: Arc<OpusCatalog>,
    packet_interval: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // No audio (e.g. Android host, video-only build): nothing to stream.
    if catalog.is_empty() {
        return Ok(());
    }

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
