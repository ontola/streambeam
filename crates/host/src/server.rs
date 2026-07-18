//! QUIC host server — pairs clients and streams video + audio.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streambeam_net::{
    Advertiser, HelloVerdict, Identity, PairingStore, advertise_ips, export_pairing_secret,
    peer_pubkey, read_msg, server_config, write_msg,
};
use streambeam_proto::PROTO_VERSION;
use streambeam_proto::control::{AudioParams, ClientMsg, HostInputMode, HostMsg};
use streambeam_proto::fec::{encode_audio_packet, encode_video_frame};
use streambeam_proto::input::InputEvent;
use streambeam_proto::pair::PairInfo;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::oneshot;

use crate::active_stream::{ActiveStreamTracker, CloseableConn};
use crate::audio::OpusCatalog;
use crate::encode::{Codec, VideoEncoder, default_bitrate_bps};
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

/// Keeps the display (and system) awake for as long as it's held, released
/// on drop. Remote streaming is exactly the scenario Windows' idle timer
/// isn't designed for: the only "activity" is a viewer on another device,
/// invisible to the local input timer that decides when to turn the display
/// off. Once that happens, DXGI Desktop Duplication starts failing
/// `AcquireNextFrame` with `ACCESS_LOST` — not a capture bug, just Desktop
/// Duplication categorically not working without an active display (see
/// `Pipeline::capture_frame`'s backoff/diagnostic for that failure mode).
/// Sunshine holds the equivalent of this for the same reason. Held for
/// `stream_video`'s lifetime — from first frame to disconnect, not from
/// host launch, so an idle host with no viewers still respects normal power
/// settings.
#[cfg(windows)]
struct KeepDisplayAwake;

#[cfg(windows)]
impl KeepDisplayAwake {
    fn new() -> Self {
        use windows::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        }
        Self
    }
}

#[cfg(windows)]
impl Drop for KeepDisplayAwake {
    fn drop(&mut self) {
        use windows::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

#[cfg(not(windows))]
struct KeepDisplayAwake;

#[cfg(not(windows))]
impl KeepDisplayAwake {
    fn new() -> Self {
        Self
    }
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
    /// Stream the bundled 440 Hz tone when no live capture feeds
    /// `audio::live` — dev/mock hosts only. Real hosts stay silent instead of
    /// playing a test tone at the viewer.
    pub audio_tone_fallback: bool,
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
    clipboard_to_client: tokio::sync::watch::Sender<(u64, String)>,
    clipboard_from_client: Mutex<Option<String>>,
    clipboard_revision: AtomicU64,
}

#[derive(Clone)]
pub struct ClientEntry {
    pub name: String,
    pub addr: SocketAddr,
    pub pubkey: [u8; 32],
    disconnect: tokio::sync::watch::Sender<bool>,
}

impl Default for HostControls {
    fn default() -> Self {
        Self::new()
    }
}

impl HostControls {
    /// Both toggles start enabled.
    pub fn new() -> Self {
        let (clipboard_to_client, _) = tokio::sync::watch::channel((0, String::new()));
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
            clipboard_to_client,
            clipboard_from_client: Mutex::new(None),
            clipboard_revision: AtomicU64::new(0),
        }
    }

    /// Publish this host's current text clipboard to active viewers.
    pub fn publish_clipboard(&self, text: String) {
        if text.len() > 60_000 || self.clipboard_to_client.borrow().1 == text {
            return;
        }
        let revision = self.clipboard_revision.fetch_add(1, Ordering::Relaxed) + 1;
        self.clipboard_to_client.send_replace((revision, text));
    }

    pub fn take_remote_clipboard(&self) -> Option<String> {
        self.clipboard_from_client.lock().unwrap().take()
    }

    fn receive_remote_clipboard(&self, text: String) {
        if text.len() <= 60_000 {
            *self.clipboard_from_client.lock().unwrap() = Some(text);
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
        entry.disconnect.send_replace(true);
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
            entry.disconnect.send_replace(true);
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

    /// Clients that currently own the media stream, rather than merely having
    /// an authenticated QUIC connection open.
    pub fn viewers(&self) -> Vec<ClientEntry> {
        let clients = self.clients.lock().unwrap().clone();
        clients
            .into_iter()
            .filter(|client| self.is_active_stream(client.addr))
            .collect()
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
    /// Client-requested bitrate floor (kbps), if any — see
    /// `ClientMsg::SetBitrate`'s handler. `stream_video`'s adaptive
    /// controller starts from this instead of the encoder's computed
    /// default when set, but still adapts up/down from there based on
    /// measured network conditions rather than pinning to it exactly.
    manual_bitrate_kbps: AtomicU32,
}

impl StreamControl {
    fn new() -> Self {
        Self {
            force_idr: AtomicBool::new(false),
            invalidate: Mutex::new(None),
            manual_bitrate_kbps: AtomicU32::new(0),
        }
    }

    fn set_manual_bitrate(&self, kbps: u32) {
        self.manual_bitrate_kbps.store(kbps, Ordering::Relaxed);
    }

    /// `0` means "no manual override yet".
    fn manual_bitrate(&self) -> u32 {
        self.manual_bitrate_kbps.load(Ordering::Relaxed)
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

/// Build the video source: real platform capture where available, else the
/// test clip. macOS deliberately returns an error rather than ever showing a
/// mock clip to a real client when Screen Recording consent is unavailable.
fn build_encoder(
    fps: u32,
    max_encode_dim: u32,
    wanted_codec: Codec,
) -> Result<Box<dyn VideoEncoder>, String> {
    // Explicit dev/test opt-in (set by the e2e harness): the platform branches
    // below otherwise wait on real capture — on macOS a Swift layer that only
    // exists inside the app bundle, never in the CLI bin.
    if std::env::var("STREAMBEAM_MOCK").is_ok_and(|v| v == "1") {
        let _ = (fps, max_encode_dim, wanted_codec);
        return Ok(Box::new(MockEncoder::bundled()));
    }
    #[cfg(windows)]
    {
        match crate::capture::DesktopEncoder::new(fps, max_encode_dim, wanted_codec) {
            Ok(enc) => {
                let (w, h) = enc.dimensions();
                crate::logging::log(&format!(
                    "desktop capture ready — {w}x{h} @ {fps}fps ({})",
                    enc.codec().wire_name()
                ));
                return Ok(Box::new(enc));
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
        // The Android host listens while idle. An authenticated SessionRequest
        // raises this demand flag; the activity opens Android's mandatory
        // MediaProjection consent UI and the capture service then feeds this
        // encoder. Keep the handshake open while the user approves it.
        // Android beaming doesn't support AV1 encode (MediaCodec surface
        // encoder path is H.264-only) — `wanted_codec` is silently ignored,
        // same as the mock clip on unsupported platforms below.
        crate::android_screen::request_capture();
        for _ in 0..300 {
            if crate::android_screen::is_ready() {
                crate::logging::log("android screen capture ready — streaming live screen");
                return Ok(Box::new(crate::android_screen::AndroidScreenEncoder));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        crate::android_screen::release_capture();
        return Err(
            "Android screen capture was not approved; accept the system prompt and retry".into(),
        );
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (fps, max_encode_dim, wanted_codec);
        // The listener stays idle until an authorized client sends its
        // SessionRequest. That request makes capture wanted; the Swift runner
        // observes it and starts ScreenCaptureKit/VideoToolbox on the main
        // thread. The longer wait covers the one-time macOS consent prompt.
        for _ in 0..300 {
            if crate::macos_screen::is_ready() {
                crate::logging::log("macOS screen capture ready — streaming live screen");
                return Ok(Box::new(crate::macos_screen::MacOsScreenEncoder));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        crate::macos_screen::release_capture();
        return Err(
            "macOS screen capture did not start; allow Screen Recording for StreamBeam and retry"
                .into(),
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        let _ = (fps, max_encode_dim, wanted_codec);
        Ok(Box::new(MockEncoder::bundled()))
    }
}

pub async fn run(config: HostConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(windows)]
    let _viiper = crate::viiper_server::ViiperGuard::ensure();

    // Windows quantizes timer waits — including every tokio sleep — to the
    // system timer resolution, 15.625ms by default. `stream_video`'s ~30ms
    // inter-frame sleep therefore rounds up to 3 ticks = 46.9ms, capping a
    // 30fps stream at a measured, rock-steady 21.3fps no matter how fast
    // capture/encode/send are (loopback baseline: 3ms of real work per
    // frame, 21.4fps anyway). 1ms resolution is what every game/streaming
    // app requests (Sunshine does exactly this); per-process since Win10
    // 2004, released automatically at process exit.
    #[cfg(windows)]
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }

    // One shared encoder feeds every connection (Desktop Duplication is
    // single-instance, so multiple simultaneous viewers aren't supported yet).
    // Built lazily on the first real `SessionRequest` (see `handle_client`),
    // not here: DXGI Desktop Duplication + the NVENC session are expensive,
    // persistent GPU resources, and used to be created unconditionally at
    // host startup — held open for the whole process lifetime even with zero
    // viewers ever connecting (wasteful, and a suspected contributor to
    // GPU/audio hiccups observed with no active stream at all). Deferring all
    // the way to the first `SessionRequest`, rather than just the first
    // connection, also means that client's requested resolution/fps is what
    // actually builds the encoder, instead of always falling back to the
    // CLI/env defaults.
    let encoder_cell: Arc<SharedEncoderCell> = Arc::new(AsyncMutex::new(None));

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
        version: PROTO_VERSION,
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

    let audio = Arc::new(if config.audio_tone_fallback {
        OpusCatalog::bundled()
    } else {
        OpusCatalog {
            packets: Vec::new(),
        }
    });

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
        let store = store.clone();
        let audio = audio.clone();
        let paired_path = paired_path.clone();
        let approval_tx = config.pairing_request_tx.clone();
        let controls = controls.clone();
        let encoder_cell = encoder_cell.clone();
        // Fallback/ceiling only now — the encoder build itself is deferred
        // further, to the first `SessionRequest` (see `handle_client`), so a
        // client's real requested resolution/fps can actually be honored
        // instead of always falling back to these CLI/env defaults.
        let default_fps = config.fps;
        let default_max_dim = config.max_encode_dim;

        tokio::spawn(async move {
            if let Err(e) = handle_client(
                incoming,
                store,
                encoder_cell,
                audio,
                default_fps,
                default_max_dim,
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

/// The shared encoder plus the request it was actually built for — so a
/// later `SessionRequest` asking for different dimensions/fps (e.g. a client
/// reconnecting after changing `StreamMode`) can tell it needs a rebuild
/// instead of silently keeping whatever the first-ever request produced.
struct CachedEncoder {
    encoder: Arc<AsyncMutex<Box<dyn VideoEncoder>>>,
    width: u16,
    height: u16,
    codec_config: Option<Vec<u8>>,
    build_fps: u32,
    build_dim: u32,
    build_codec: Codec,
}

type SharedEncoderCell = AsyncMutex<Option<CachedEncoder>>;

/// Longest side a client's `SessionRequest.video.{max_width,max_height}` can
/// push the encoder to, and the fastest `max_fps` it can request — independent
/// of `default_max_dim`/`default_fps` (the CLI/env admin defaults, now only a
/// fallback for a client that sends degenerate/zero caps). FEC's own shard-
/// count ceiling is nowhere near this (`MAX_DATA_SHARDS` supports ~19.6MB per
/// frame), so this exists purely to keep a broken or hostile client from
/// requesting something absurd, not to protect FEC.
const MAX_CLIENT_ENCODE_DIM: u32 = 3840;
const MAX_CLIENT_FPS: u32 = 120;

async fn handle_client(
    incoming: quinn::Incoming,
    store: Arc<Mutex<PairingStore>>,
    encoder_cell: Arc<SharedEncoderCell>,
    audio: Arc<OpusCatalog>,
    default_fps: u32,
    default_max_dim: u32,
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
    let (disconnect_tx, mut disconnect_rx) = tokio::sync::watch::channel(false);
    controls.add_client(ClientEntry {
        name: client_name.clone(),
        addr: conn.remote_address(),
        pubkey: client_pk,
        disconnect: disconnect_tx,
    });
    let _client_guard = ClientGuard {
        controls: controls.clone(),
        addr: conn.remote_address(),
    };

    let input_conn = conn.clone();
    let input_controls = controls.clone();
    tokio::spawn(async move {
        loop {
            match input_conn.accept_uni().await {
                Ok(recv) => {
                    log!("input stream opened");
                    if let Err(e) = handle_input_stream(recv, input_controls.clone()).await {
                        log!("input stream ended: {e}");
                    }
                }
                Err(_) => break,
            }
        }
    });

    let stream_ctl = Arc::new(StreamControl::new());
    let mut streaming = false;
    // Set once `SessionRequest` resolves the (possibly first-ever) encoder
    // build — `SetBitrate` and anything else needing the encoder later reads
    // it from here rather than a value resolved before we knew what the
    // client actually wanted.
    let mut resolved: Option<(Arc<AsyncMutex<Box<dyn VideoEncoder>>>, u16, u16)> = None;
    let mut last_idr = std::time::Instant::now();
    // RFI's invalidate IS now cheaper to service than a full IDR on the
    // NVENC path (`NvEncInvalidateRefFrames` — no keyframe burst, no session
    // recreate; see `stream_video`'s frame-id→timestamp mapping), but it
    // still *falls back* to `force_idr()` (a full session recreate) whenever
    // the range can't be mapped or the driver call fails, and the software/
    // mock encoders always take that fallback. Keep the 250ms gate: an
    // ungated 60ms one historically let a client stuck in a
    // request-recover-request loop (reacting to the recreate's own brief
    // stall as if it were more loss) tear down and rebuild the encoder up to
    // ~16x/sec — the "fps very inconsistent" / "data rate collapses" reports.
    let mut last_invalidate = std::time::Instant::now();
    let mut clipboard_rx = controls.clipboard_to_client.subscribe();

    loop {
        let msg: Option<ClientMsg> = tokio::select! {
            _ = conn.closed() => break,
            read = read_msg(&mut recv, &mut buf) => match read {
                Ok(m) => Some(m),
                Err(_) => break,
            },
            changed = clipboard_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                if streaming {
                    let text = clipboard_rx.borrow_and_update().1.clone();
                    write_msg(&mut send, &HostMsg::ClipboardText { text }).await?;
                }
                None
            }
            changed = disconnect_rx.changed() => {
                if changed.is_err() || *disconnect_rx.borrow_and_update() {
                    // Tell the viewer why media is ending on the reliable
                    // control stream. Closing QUIC alone can leave a client
                    // displaying its last decoded frame until its media-read
                    // timeout notices the lost producer.
                    let _ = write_msg(&mut send, &HostMsg::Bye).await;
                    // Give Quinn one scheduling turn to put Bye on the wire
                    // before CONNECTION_CLOSE supersedes pending stream data.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    break;
                }
                None
            }
        };
        let Some(msg) = msg else { continue };

        match msg {
            ClientMsg::SessionRequest { video, .. } => {
                #[cfg(target_os = "macos")]
                crate::macos_screen::set_requested_display(video.display_id);
                // Honor this client's real display caps, clamped to a sanity
                // ceiling independent of the CLI/env admin defaults (which
                // only apply when a client sends degenerate/zero caps —
                // shouldn't happen with a real device, but devtools/older
                // clients might).
                let want_dim = u32::from(video.max_width.max(video.max_height));
                let want_fps = u32::from(video.max_fps);
                let build_dim = if want_dim > 0 {
                    want_dim.min(MAX_CLIENT_ENCODE_DIM)
                } else {
                    default_max_dim
                };
                let build_fps = if want_fps > 0 {
                    want_fps.min(MAX_CLIENT_FPS)
                } else {
                    default_fps
                };
                // The native SCK/VideoToolbox producer currently runs at a
                // fixed 30 fps. Do not advertise a client-requested 120 fps
                // in SessionAccept while only supplying 30; that makes its
                // pacing/backlog heuristics reason from false metadata.
                #[cfg(target_os = "macos")]
                let build_fps = build_fps.min(crate::macos_screen::CAPTURE_FPS);
                // AV1's better compression is only worth it if this client
                // can actually decode it — `build_encoder` still isn't a
                // hard promise (falls back to H.264 if this host's GPU
                // doesn't support AV1 NVENC either), but there's no point
                // even trying for a client that never asked for it.
                let wanted_codec = if video.av1 { Codec::Av1 } else { Codec::H264 };

                // Claim the shared encoder *before* deciding whether to
                // rebuild it — this waits for a still-live previous
                // connection's `stream_video` task to fully exit first (see
                // `ActiveStreamTracker::start_streaming`), so we never
                // rebuild out from under a task that's still mid-iteration
                // on the old one. Idempotent for a repeat `SessionRequest` on
                // the same connection.
                controls.start_streaming(conn.remote_address(), &conn).await;

                #[cfg(target_os = "macos")]
                crate::macos_screen::request_capture(build_dim);
                #[cfg(target_os = "android")]
                crate::android_screen::request_capture();

                let mut cell = encoder_cell.lock().await;
                #[cfg(target_os = "macos")]
                let source_unavailable = !crate::macos_screen::is_ready();
                #[cfg(target_os = "android")]
                let source_unavailable = !crate::android_screen::is_ready();
                #[cfg(not(any(target_os = "macos", target_os = "android")))]
                let source_unavailable = false;
                let needs_rebuild = source_unavailable
                    || !matches!(
                        cell.as_ref(),
                        Some(c) if c.build_fps == build_fps && c.build_dim == build_dim
                            && c.build_codec == wanted_codec
                    );
                if needs_rebuild {
                    // Rebuilding mid-connection (a client re-sending
                    // `SessionRequest` with new caps on an already-streaming
                    // connection, rather than reconnecting) would leave the
                    // already-spawned `stream_video` task holding a clone of
                    // the *old* encoder Arc — it won't pick up the new one.
                    // Not a real scenario today: every client here only ever
                    // sends `SessionRequest` once, right after connecting,
                    // and changes caps by fully reconnecting (see
                    // `_reconnectForNewCaps` in the Flutter app).
                    let encoder = build_encoder(build_fps, build_dim, wanted_codec)?;
                    let (width, height) = encoder.dimensions();
                    let codec_config = encoder.codec_config();
                    let build_codec = encoder.codec();
                    log!(
                        "encoder {} — {width}x{height} @ {build_fps}fps, {} (requested by {client_name}: {}x{} @ {}fps, av1={}), {} opus packets",
                        if cell.is_some() { "rebuilt" } else { "ready" },
                        build_codec.wire_name(),
                        video.max_width,
                        video.max_height,
                        video.max_fps,
                        video.av1,
                        OpusCatalog::bundled().len()
                    );
                    *cell = Some(CachedEncoder {
                        encoder: Arc::new(AsyncMutex::new(encoder)),
                        width: width as u16,
                        height: height as u16,
                        codec_config,
                        build_fps,
                        build_dim,
                        build_codec,
                    });
                }
                let cached = cell.as_ref().expect("just populated above if empty");
                let (encoder, width, height, codec_config, build_codec) = (
                    cached.encoder.clone(),
                    cached.width,
                    cached.height,
                    cached.codec_config.clone(),
                    cached.build_codec,
                );
                drop(cell);
                resolved = Some((encoder.clone(), width, height));

                // Don't rely solely on the value captured when the encoder
                // was built (`codec_config`, above) — some encoders don't
                // have it available immediately: hardware MFTs need a real
                // keyframe produced first (see capture.rs), and on Android
                // the MediaCodec CSD (SPS/PPS) can arrive shortly after the
                // host starts. Start streaming *first* (below) so
                // `stream_video`'s own naturally-paced, timeout-protected
                // `encode_next()` loop is what actually drives the encoder —
                // then just wait (asynchronously, not blocking a worker
                // thread) for `codec_config` to show up as a side effect.
                // Driving `encode_next()` synchronously here ourselves was
                // tried and reverted: it can't tell "screen hasn't changed
                // yet" from "encoder stalled", and blocked the whole
                // connection for 50+ seconds in practice on this machine's
                // hardware encoder — worse than the bug it was meant to fix.
                if !streaming {
                    streaming = true;
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
                            media_controls,
                            media_addr,
                            width.into(),
                            height.into(),
                            build_fps,
                        )
                        .await
                        {
                            Ok(()) => log!("video stream ended: connection closed"),
                            Err(e) => log!("video stream ended: {e}"),
                        }
                    });
                    controls.set_stream_task(conn.remote_address(), video_task);
                    // Drop packets a previous session encoded but never sent,
                    // so the new viewer doesn't start with a stale burst.
                    crate::audio::live::reset();
                    let audio_conn = conn.clone();
                    let audio_cat = audio.clone();
                    let audio_controls = controls.clone();
                    let audio_interval = Duration::from_millis(10);
                    tokio::spawn(async move {
                        if let Err(e) =
                            stream_audio(audio_conn, audio_cat, audio_controls, audio_interval)
                                .await
                        {
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
                    #[cfg(target_os = "macos")]
                    write_msg(
                        &mut send,
                        &HostMsg::DisplayList {
                            displays: crate::macos_screen::displays(),
                            selected_id: crate::macos_screen::selected_display(),
                        },
                    )
                    .await?;
                    write_msg(&mut send, &HostMsg::CodecConfig { data: data.clone() }).await?;
                }
                write_msg(
                    &mut send,
                    &HostMsg::SessionAccept {
                        session_id: 1,
                        codec: build_codec.wire_name().into(),
                        width,
                        height,
                        fps: build_fps.min(255) as u8,
                        audio: AudioParams {
                            sample_rate: 48_000,
                            channels: 2,
                            frame_ms: 10,
                        },
                        input_mode: if cfg!(target_os = "android") {
                            HostInputMode::Touch
                        } else {
                            HostInputMode::Mouse
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
                    if let Some((encoder, ..)) = &resolved {
                        encoder.lock().await.force_idr();
                    }
                    last_idr = std::time::Instant::now();
                    log!("IDR scheduled");
                }
            }
            ClientMsg::InvalidateRefFrames {
                first_frame,
                last_frame,
            } => {
                if last_invalidate.elapsed() >= Duration::from_millis(250) {
                    stream_ctl.request_invalidate(first_frame, last_frame);
                    last_invalidate = std::time::Instant::now();
                    log!("invalidate ref frames scheduled: [{first_frame}, {last_frame}]");
                }
            }
            ClientMsg::SetBitrate { kbps } => {
                // A manual override from the client, layered under the
                // adaptive controller in `stream_video`: it becomes that
                // controller's new floor/starting point rather than fighting
                // it — see `StreamControl::manual_bitrate_kbps`.
                stream_ctl.set_manual_bitrate(kbps);
                if let Some((encoder, ..)) = &resolved {
                    encoder.lock().await.set_bitrate(kbps);
                }
                log!("client requested bitrate {kbps} kbps");
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
    controls: Arc<HostControls>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = Vec::new();
    let result = async {
        loop {
            let event: InputEvent = read_msg(&mut recv, &mut buf).await?;
            match event {
                InputEvent::ClipboardText { text } => controls.receive_remote_clipboard(text),
                other => crate::input_inject::apply(other),
            }
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
    controls: Arc<HostControls>,
    addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "macos")]
    struct CaptureLease;
    #[cfg(target_os = "macos")]
    impl Drop for CaptureLease {
        fn drop(&mut self) {
            crate::macos_screen::release_capture();
        }
    }
    #[cfg(target_os = "macos")]
    let _capture_lease = CaptureLease;
    #[cfg(target_os = "android")]
    struct CaptureLease;
    #[cfg(target_os = "android")]
    impl Drop for CaptureLease {
        fn drop(&mut self) {
            crate::android_screen::release_capture();
        }
    }
    #[cfg(target_os = "android")]
    let _capture_lease = CaptureLease;
    // Dropped at every `return`/`break` below via normal Rust scope rules —
    // released the moment this session ends, one way or another.
    let _keep_awake = KeepDisplayAwake::new();
    // Paced from the actual negotiated `fps` for *this* connection (the
    // client's real requested rate, clamped in the `SessionRequest` handler
    // above — see `MAX_CLIENT_FPS`), not a host-wide default computed once
    // at startup. That used to be the whole reason a client could ask for
    // 120fps, have the encoder actually built at 120fps, and still only
    // ever see 30 — this loop's own pacing interval never moved off the
    // CLI/env default in the first place, silently overriding request.
    let frame_interval = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
    let max_datagram = conn
        .max_datagram_size()
        .unwrap_or(1200 + streambeam_proto::media::MEDIA_HEADER_LEN);
    let mut frame_id: u16 = 0;
    // Wire `frame_id` → encoder-internal timestamp for each recently sent
    // frame, so a client `InvalidateRefFrames { first, last }` range can be
    // resolved into the `EncodedFrame::timestamp` values NVENC names its DPB
    // entries by (`VideoEncoder::invalidate_ref_timestamps`). Only the send
    // loop knows both halves of that mapping, which is why it lives here and
    // not in the encoder. Bounded well past any real DPB depth; a lookup
    // miss just falls back to a forced IDR. Cleared on every forced IDR:
    // an NVENC forced IDR is a session recreate that restarts timestamps
    // from 0, so pre-IDR entries would alias new frames.
    const SENT_FRAME_MAP_LEN: usize = 128;
    let mut sent_frames: std::collections::VecDeque<(u16, u64)> =
        std::collections::VecDeque::with_capacity(SENT_FRAME_MAP_LEN);
    // How many upcoming frames get a full OBU-layout trace in the log (see
    // the diagnostics block in the send loop). Starts armed for the session
    // opening; re-armed after recovery/reconfigure events so the log shows
    // what those actually do to the bitstream structure.
    let mut obu_trace_left: u32 = 6;
    // Debug tee of the exact encoded bitstream this session sends (raw
    // concatenated access units — for AV1 that's a low-overhead OBU stream
    // ffmpeg can decode directly). Lets a reference decoder rule the host's
    // output in or out when a client shows corruption the host log can't
    // explain. Opt-in via env; truncates per session.
    let mut video_dump = std::env::var("STREAMBEAM_DUMP_VIDEO")
        .ok()
        .and_then(|p| std::fs::File::create(p).ok());
    // Rolling per-report-window timing, to find where per-frame latency goes:
    // waiting on the encoder (capture+encode+channel round trip), FEC sharding,
    // or actually pushing shards onto the wire.
    let mut encode_wait_us: u128 = 0;
    let mut shard_us: u128 = 0;
    let mut send_us: u128 = 0;
    let mut window_start = std::time::Instant::now();
    // Wire-level accounting for the same window, so host-sent bytes/datagrams
    // can be compared 1:1 against what the client reports receiving — the
    // difference IS the network loss rate, with no guesswork in between.
    let mut window_datagrams: u64 = 0;
    let mut window_bytes: u64 = 0;
    // Iterations where the encoder had nothing (static screen / capture
    // timeout). Distinguishes "host idled because nothing changed on screen"
    // from "host was slow" when the fps number alone looks bad.
    let mut window_empty_ticks: u32 = 0;
    // Frames actually sent in the current wall-clock window (see the
    // reporting block below) — no longer always 30, now that a window is a
    // fixed ~1s of real time rather than a fixed frame count.
    let mut window_frame_count: u32 = 0;
    // quinn path counters are cumulative for the connection; keep the last
    // reading so the log line can show per-window deltas.
    let mut last_lost_packets: u64 = 0;
    let mut last_congestion_events: u64 = 0;
    // Adaptive bitrate: back off hard and fast on real loss/congestion (a
    // burst of dropped packets means we're already overshooting what the
    // path can carry right now), ease back up slowly on sustained clean
    // windows (headroom should be earned back gradually, not assumed). A
    // manual `ClientMsg::SetBitrate` becomes the new floor this still
    // adapts around, not a value that pins the encoder and fights the
    // controller every window.
    let manual_floor = ctl.manual_bitrate();
    let max_bitrate_kbps = default_bitrate_bps(width, height, fps) / 1000;
    // Start well under the ceiling instead of *at* it: jumping straight to
    // the theoretical max on connect — before anything has actually proven
    // the link can sustain that — routinely blew straight through real WiFi
    // capacity (hundreds of lost packets within the first couple of
    // seconds), then kept recreating the NVENC session (see below) on the
    // resulting cascade of backoffs. Climbing from a conservative floor
    // via the existing "5 clean windows -> +10%" mechanism finds the real
    // ceiling without ever overshooting it violently at connect time.
    let mut current_bitrate_kbps = if manual_floor > 0 {
        manual_floor
    } else {
        (max_bitrate_kbps / 4).max(4_000)
    };
    // Last value actually pushed to the encoder — see `MIN_RECREATE_INTERVAL`.
    let mut applied_bitrate_kbps = current_bitrate_kbps;
    let min_bitrate_kbps = 1_000;
    let mut clean_windows = 0u32;
    // A session recreate is expensive (fresh NVENC session + a forced IDR —
    // itself a large burst right when backing off from congestion is trying
    // to send *less*), so only actually apply a backoff-triggered bitrate
    // change this often — a cascade of consecutive bad windows still backs
    // off its *target* every window (so it doesn't lag reality), it just
    // doesn't tear down/rebuild the encoder for every single one of them.
    // 800ms was still short enough that the adaptive controller's own
    // "ease up 10% every 5 clean windows" ratchet — combined with
    // `InvalidateRefFrames`/`RequestIdr` (both *also* full recreates, see
    // above) — kept the encoder almost perpetually mid-recreate rather than
    // actually settling: recreate → brief stall/burst → looks like fresh
    // loss/congestion next window → another backoff → another recreate.
    // Widened so the path gets a real window to prove in before the next
    // expensive rebuild.
    const MIN_RECREATE_INTERVAL: Duration = Duration::from_secs(4);
    {
        let mut enc = encoder.lock().await;
        enc.set_bitrate(current_bitrate_kbps);
    }
    let mut last_bitrate_recreate = std::time::Instant::now();

    // Pace against an absolute per-frame schedule (`next_frame`), not a
    // relative sleep per iteration: with relative sleeps every bit of
    // overshoot — timer quantization, a slow encode, scheduler jitter —
    // compounds into the next frame's start time and permanently lowers fps.
    // An absolute deadline absorbs jitter instead: a frame that ran late is
    // followed by a shorter wait, not another full interval. (This only
    // works on Windows together with the `timeBeginPeriod(1)` call in
    // `run()` — at the default 15.625ms timer resolution even sleep_until
    // overshoots by up to a whole tick per frame.)
    let mut next_frame = tokio::time::Instant::now();

    loop {
        tokio::time::sleep_until(next_frame).await;
        let now = tokio::time::Instant::now();
        // Next slot is one interval after this one — unless we've already
        // fallen more than an interval behind (slow encode, long stall), in
        // which case re-anchor to now rather than bursting to catch up.
        next_frame = (next_frame + frame_interval).max(now);

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

        // Drain both signals every iteration so neither goes stale, but only
        // act on one: a pending IDR is the stronger recovery and makes any
        // pending invalidate moot (the keyframe it forces doesn't reference
        // anything the invalidate range was trying to avoid referencing).
        let force_idr = ctl.take_force_idr();
        let invalidate = ctl.take_invalidate();
        if force_idr {
            let mut enc = encoder.lock().await;
            enc.force_idr();
            sent_frames.clear();
        } else if let Some((first, last)) = invalidate {
            // Resolve the client's lost wire-frame range into encoder
            // timestamps (see `sent_frames`). The driver handles transitive
            // damage itself — NVENC's docs: frames *reconstructed from* an
            // invalidated frame are invalidated too, and if no valid
            // reference remains the next frame is intra-coded — so passing
            // exactly the reported range is sufficient. An unmappable or
            // absurdly wide range (nothing that old is in any DPB) falls
            // back to the forced IDR that used to service every invalidate.
            let span = usize::from(last.wrapping_sub(first)) + 1;
            let timestamps: Vec<u64> = (0..span as u16)
                .filter_map(|off| {
                    let id = first.wrapping_add(off);
                    sent_frames.iter().find(|(fid, _)| *fid == id).map(|(_, ts)| *ts)
                })
                .collect();
            let mut enc = encoder.lock().await;
            obu_trace_left = obu_trace_left.max(2);
            if span <= 64 && timestamps.len() == span {
                enc.invalidate_ref_timestamps(&timestamps);
            } else {
                log!(
                    "invalidate [{first}, {last}]: {}/{span} frames mappable — forcing IDR instead",
                    timestamps.len()
                );
                enc.force_idr();
                sent_frames.clear();
            }
        }

        let t_encode = std::time::Instant::now();
        let frame = {
            let mut enc = encoder.lock().await;
            enc.encode_next().map_err(|e| e.to_string())?
        };
        encode_wait_us += t_encode.elapsed().as_micros();
        // No frame this tick (e.g. screen unchanged) — next slot will retry.
        let Some(frame) = frame else {
            window_empty_ticks += 1;
            continue;
        };

        // Diagnostics for the Windows→Mac AV1 corruption hunt (2026-07-18):
        // keyframes are far too rare for the 1/s sampled log line to ever
        // catch one, and the client's redundant-header stripping makes
        // structural assumptions about the temporal unit that no log has
        // ever verified against what NVENC actually emits. Log every
        // keyframe, and OBU-trace the first few frames per session plus the
        // frames right after a recovery/reconfigure event (armed below).
        if frame.keyframe {
            log!("sent keyframe {frame_id} ({} bytes)", frame.data.len());
        }
        if let Some(f) = video_dump.as_mut() {
            use std::io::Write;
            let _ = f.write_all(&frame.data);
        }
        if obu_trace_left > 0 {
            obu_trace_left -= 1;
            log!(
                "frame {frame_id} trace: keyframe={}, {} bytes, obus=[{}]",
                frame.keyframe,
                frame.data.len(),
                streambeam_core::av1_obu_type_summary(&frame.data)
            );
        }

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
                continue;
            }
        };
        shard_us += t_shard.elapsed().as_micros();

        let t_send = std::time::Instant::now();
        // A keyframe is a dozens-of-shards burst (a 40-90KB I-frame at
        // ~1200 bytes/shard is 30-80 datagrams) — `send_datagram_wait` only
        // blocks on quinn's own send buffer, which is generous enough that
        // the whole burst leaves this loop in well under a millisecond. On
        // WiFi specifically that's a real problem even on an otherwise-good
        // link: the AP's own transmit queue/channel-access arbitration has
        // nowhere near that instantaneous throughput, so the tail of the
        // burst gets dropped locally, independent of the path's steady-state
        // bandwidth — observed correlating almost every large keyframe with
        // a spike of 100-250+ lost packets that then triggers an adaptive
        // bitrate backoff, even on a strong/wired link. Spreading a large
        // burst over a few milliseconds (still far below one frame interval
        // even at 60fps) costs negligible latency but gives the local link
        // room to actually drain between chunks.
        const PACE_CHUNK: usize = 8;
        const PACE_DELAY: Duration = Duration::from_micros(200);
        for (i, shard) in shards.into_iter().enumerate() {
            if i > 0 && i % PACE_CHUNK == 0 {
                tokio::time::sleep(PACE_DELAY).await;
            }
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            window_datagrams += 1;
            window_bytes += dg.len() as u64;
            if conn.send_datagram_wait(dg.into()).await.is_err() {
                return Ok(());
            }
        }
        send_us += t_send.elapsed().as_micros();
        window_frame_count += 1;

        // Wall-clock cadence, not frame count: at 30fps, 30 frames and 1
        // second were the same thing, but once fps became genuinely
        // client-negotiated (client can now ask for 60-120fps) a
        // frame-count trigger fires proportionally more often — every
        // ~0.25-0.5s at 60-120fps instead of ~1s. The adaptive controller
        // below reacts to a single bad window immediately (by design — see
        // its comment), so quadrupling how often it evaluates quadruples
        // how often ordinary jitter looks like "real" congestion, which
        // was collapsing the bitrate (and the NVENC session recreate that
        // comes with every change) far more often than intended.
        if window_start.elapsed() >= Duration::from_secs(1) {
            let window_secs = window_start.elapsed().as_secs_f64().max(0.001);
            let qs = conn.stats();
            let new_lost = qs.path.lost_packets.saturating_sub(last_lost_packets);
            let new_congestion = qs
                .path
                .congestion_events
                .saturating_sub(last_congestion_events);

            // Adaptive bitrate: any loss/congestion this window means the
            // path is already carrying more than it can — back off hard
            // (25%) and reset the clean-window streak immediately, so a
            // single bad window can't get diluted by good ones around it.
            // Otherwise, after several consecutive clean windows, ease up
            // (10%) — slow enough that a marginal path oscillates gently
            // around its real ceiling instead of sawtoothing.
            let manual_floor = ctl.manual_bitrate();
            let floor = if manual_floor > 0 {
                manual_floor
            } else {
                min_bitrate_kbps
            };
            if new_lost > 0 || new_congestion > 0 {
                clean_windows = 0;
                current_bitrate_kbps = (current_bitrate_kbps * 3 / 4).max(floor);
            } else {
                clean_windows += 1;
                // Widened from 5: at ~1 recreate per ease-up (see
                // `MIN_RECREATE_INTERVAL`'s comment on why every apply costs
                // a full NVENC session rebuild), ratcheting up every 5
                // clean seconds meant an already-healthy connection kept
                // recreating just to creep toward a ceiling it didn't need
                // for the content on screen. 15s of sustained clean
                // headroom is still responsive, just far less trigger-happy.
                if clean_windows >= 15 {
                    clean_windows = 0;
                    current_bitrate_kbps = (current_bitrate_kbps * 11 / 10).min(max_bitrate_kbps);
                }
            }
            // The *target* above always tracks reality every window (so a
            // cascade of bad windows doesn't lag behind real congestion) —
            // but actually applying it means a session recreate (fresh
            // NVENC session + a forced IDR, itself a large burst), so that
            // part is rate-limited: no more than one per
            // `MIN_RECREATE_INTERVAL`, applying whatever the target has
            // drifted to by the time the cooldown clears rather than
            // recreating on every single intermediate step.
            if current_bitrate_kbps != applied_bitrate_kbps
                && last_bitrate_recreate.elapsed() >= MIN_RECREATE_INTERVAL
            {
                applied_bitrate_kbps = current_bitrate_kbps;
                encoder.lock().await.set_bitrate(applied_bitrate_kbps);
                last_bitrate_recreate = std::time::Instant::now();
                obu_trace_left = obu_trace_left.max(2);
            }

            let frames_this_window = u128::from(window_frame_count.max(1));
            log!(
                "sent frame {frame_id} ({} bytes, keyframe={}) — last {window_frame_count} frames: {:.1} fps, avg encoder wait {} ms, FEC shard {} ms, send {} ms | wire: {} dgrams, {:.0} kbps, {} empty ticks | path: rtt {} ms, cwnd {}, +{new_lost} lost pkts, +{new_congestion} congestion | bitrate target {current_bitrate_kbps} kbps (applied {applied_bitrate_kbps} kbps)",
                frame.data.len(),
                frame.keyframe,
                f64::from(window_frame_count) / window_secs,
                encode_wait_us / frames_this_window / 1000,
                shard_us / frames_this_window / 1000,
                send_us / frames_this_window / 1000,
                window_datagrams,
                (window_bytes as f64 * 8.0 / 1000.0) / window_secs,
                window_empty_ticks,
                qs.path.rtt.as_millis(),
                qs.path.cwnd,
            );
            last_lost_packets = qs.path.lost_packets;
            last_congestion_events = qs.path.congestion_events;
            encode_wait_us = 0;
            shard_us = 0;
            send_us = 0;
            window_datagrams = 0;
            window_bytes = 0;
            window_empty_ticks = 0;
            window_frame_count = 0;
            window_start = std::time::Instant::now();
        }

        if sent_frames.len() == SENT_FRAME_MAP_LEN {
            sent_frames.pop_front();
        }
        sent_frames.push_back((frame_id, frame.timestamp));
        frame_id = frame_id.wrapping_add(1);
    }
    Ok(())
}

async fn stream_audio(
    conn: quinn::Connection,
    catalog: Arc<OpusCatalog>,
    controls: Arc<HostControls>,
    packet_interval: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Video-only build with no tone fallback: nothing will ever be queued.
    if catalog.is_empty() && !cfg!(feature = "audio") {
        return Ok(());
    }

    let addr = conn.remote_address();
    let mut packet_id: u16 = 0;
    let mut idx = 0usize;
    let mut ticker = tokio::time::interval(packet_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        if conn.close_reason().is_some() {
            break;
        }
        // The live queue is shared; only the connection that owns the stream
        // may drain it, or a superseded session would steal packets.
        if !controls.is_active_stream(addr) {
            break;
        }

        if crate::audio::live::is_active() {
            // Capture is real-time, so normally one packet per tick; after a
            // hiccup a couple queued up — send them all to catch back up.
            while let Some(opus) = crate::audio::live::take_packet() {
                let shard = encode_audio_packet(packet_id, &opus, capture_ts_us());
                let mut dg = shard.header.encode().to_vec();
                dg.extend_from_slice(&shard.payload);
                if conn.send_datagram_wait(dg.into()).await.is_err() {
                    return Ok(());
                }
                packet_id = packet_id.wrapping_add(1);
            }
        } else if !catalog.is_empty() {
            let opus = &catalog.packets[idx % catalog.len()];
            let shard = encode_audio_packet(packet_id, opus, capture_ts_us());
            let mut dg = shard.header.encode().to_vec();
            dg.extend_from_slice(&shard.payload);
            if conn.send_datagram_wait(dg.into()).await.is_err() {
                return Ok(());
            }
            idx += 1;
            packet_id = packet_id.wrapping_add(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod clipboard_tests {
    use super::{ClientEntry, HostControls};

    #[tokio::test]
    async fn disconnect_signals_the_connection_handler() {
        let controls = HostControls::new();
        let addr = "127.0.0.1:47990".parse().unwrap();
        let (disconnect, mut requested) = tokio::sync::watch::channel(false);
        controls.add_client(ClientEntry {
            name: "viewer".into(),
            addr,
            pubkey: [7; 32],
            disconnect,
        });

        assert!(controls.disconnect(addr));
        requested.changed().await.unwrap();
        assert!(*requested.borrow_and_update());
    }

    #[tokio::test]
    async fn host_clipboard_is_published_to_viewers() {
        let controls = HostControls::new();
        let mut receiver = controls.clipboard_to_client.subscribe();

        controls.publish_clipboard("from host".into());

        receiver.changed().await.unwrap();
        assert_eq!(receiver.borrow_and_update().1, "from host");
    }

    #[test]
    fn client_clipboard_can_be_taken_once() {
        let controls = HostControls::new();

        controls.receive_remote_clipboard("from client".into());

        assert_eq!(
            controls.take_remote_clipboard().as_deref(),
            Some("from client")
        );
        assert_eq!(controls.take_remote_clipboard(), None);
    }

    #[test]
    fn oversized_clipboards_are_rejected_in_both_directions() {
        let controls = HostControls::new();
        let oversized = "x".repeat(60_001);

        controls.publish_clipboard(oversized.clone());
        controls.receive_remote_clipboard(oversized);

        assert_eq!(controls.clipboard_to_client.borrow().0, 0);
        assert_eq!(controls.take_remote_clipboard(), None);
    }
}
