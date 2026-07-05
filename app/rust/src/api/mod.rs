use std::time::Duration;

use streambeam_core::{
    FrameReassembler, SavedHost, clear_saved_host, connect_as_identity, load_saved_host,
    save_saved_host,
};
use streambeam_proto::media::{CHANNEL_AUDIO, MediaHeader};

use crate::store::{client_identity_path, saved_host_path};

#[flutter_rust_bridge::frb(sync)]
pub fn playback_frames_rendered() -> u64 {
    crate::playback::frames_rendered()
}

#[derive(Debug, Clone)]
pub struct PlaybackDiagnostics {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: String,
    pub bytes_received: u64,
    pub frames_rendered: u64,
}

#[flutter_rust_bridge::frb(sync)]
pub fn playback_diagnostics() -> PlaybackDiagnostics {
    PlaybackDiagnostics {
        width: crate::playback::session_width(),
        height: crate::playback::session_height(),
        fps: crate::playback::session_fps(),
        codec: crate::playback::session_codec(),
        bytes_received: crate::playback::bytes_received(),
        frames_rendered: crate::playback::frames_rendered(),
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_move(dx: i16, dy: i16) {
    #[cfg(target_os = "android")]
    crate::input::send(streambeam_proto::input::InputEvent::MouseMove { dx, dy });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_scroll(dx: i16, dy: i16) {
    #[cfg(target_os = "android")]
    crate::input::send(streambeam_proto::input::InputEvent::Scroll { dx, dy });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_abs(x: u16, y: u16, ref_w: u16, ref_h: u16) {
    #[cfg(target_os = "android")]
    crate::input::send(streambeam_proto::input::InputEvent::MouseAbs { x, y, ref_w, ref_h });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_click(button: u8) {
    #[cfg(target_os = "android")]
    {
        use streambeam_proto::input::InputEvent;
        crate::input::send(InputEvent::MouseButton { button, down: true });
        crate::input::send(InputEvent::MouseButton {
            button,
            down: false,
        });
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_gamepad_plug(pad: u8, connected: bool) {
    #[cfg(target_os = "android")]
    crate::input::send(streambeam_proto::input::InputEvent::GamepadPlug {
        pad: pad.min(3),
        connected,
    });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_gamepad_state(
    pad: u8,
    buttons: u16,
    lt: u8,
    rt: u8,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
) {
    #[cfg(target_os = "android")]
    crate::input::send(streambeam_proto::input::InputEvent::GamepadState {
        pad: pad.min(3),
        buttons,
        lt,
        rt,
        lx,
        ly,
        rx,
        ry,
    });
}

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
    #[cfg(target_os = "android")]
    {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("StreamBeam"),
        );
    }
}

/// Call once at startup with the app documents directory (Android/iOS).
#[flutter_rust_bridge::frb]
pub fn set_app_data_dir(dir: String) {
    crate::store::set_data_dir(&dir);
    let _ = std::fs::create_dir_all(&dir);
}

#[derive(Debug, Clone)]
pub struct StreamStats {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: String,
    pub video_frames: u64,
    pub audio_packets: u64,
}

#[flutter_rust_bridge::frb(sync)]
pub fn streambeam_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

#[flutter_rust_bridge::frb(sync)]
pub fn saved_host_label() -> Option<String> {
    load_saved_host(&saved_host_path())
        .ok()
        .flatten()
        .map(|h| h.host_name)
}

/// Saved host details for the device list (shown even when offline).
#[derive(Debug, Clone)]
pub struct SavedHostInfo {
    pub name: String,
    pub port: u16,
    pub addrs: Vec<String>,
    pub host_pubkey: Vec<u8>,
}

#[flutter_rust_bridge::frb(sync)]
pub fn load_saved_host_info() -> Option<SavedHostInfo> {
    load_saved_host(&saved_host_path()).ok().flatten().map(|h| {
        let primary = h.primary_addr();
        SavedHostInfo {
            name: h.host_name,
            port: primary.map(|a| a.port()).unwrap_or(47989),
            addrs: h.addrs.iter().map(|a| a.ip().to_string()).collect(),
            host_pubkey: h.host_pubkey.to_vec(),
        }
    })
}

/// Persist host pubkey/addrs from a scanned `sbm1:` payload for later reconnect.
#[flutter_rust_bridge::frb]
pub fn save_host_from_payload(payload: String) -> Result<(), String> {
    let host = SavedHost::from_pair_payload(&payload).map_err(|e| e.to_string())?;
    save_saved_host(&saved_host_path(), &host).map_err(|e| e.to_string())
}

/// Remove the saved host so Watch / reconnect is unavailable until paired again.
#[flutter_rust_bridge::frb]
pub fn forget_saved_host() -> Result<(), String> {
    clear_saved_host(&saved_host_path()).map_err(|e| e.to_string())
}

/// LAN hosts advertising via mDNS.
#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    pub name: String,
    pub port: u16,
    pub addrs: Vec<String>,
    /// Host Ed25519 public key (32 bytes) from mDNS; empty if host is too old.
    pub host_pubkey: Vec<u8>,
}

#[flutter_rust_bridge::frb]
pub async fn discover_hosts(timeout_ms: u32) -> Result<Vec<DiscoveredHost>, String> {
    use std::time::Duration;

    let timeout = Duration::from_millis(u64::from(timeout_ms.max(1000)));
    log::info!("discover_hosts: browsing for {timeout_ms} ms");
    let hosts = streambeam_net::browse(timeout).map_err(|e| e.to_string())?;
    log::info!("discover_hosts: found {} host(s)", hosts.len());
    Ok(hosts
        .into_iter()
        .map(|h| DiscoveredHost {
            name: h.name,
            port: h.port,
            addrs: h.addrs.iter().map(|a| a.to_string()).collect(),
            host_pubkey: h.host_pubkey.map(|pk| pk.to_vec()).unwrap_or_default(),
        })
        .collect())
}

/// Connect to a host found on the LAN (QR not required; approve on PC if new).
#[flutter_rust_bridge::frb]
pub async fn play_discovered_host(
    host_name: String,
    host_pubkey: Vec<u8>,
    addr: String,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        return crate::android::play::run_discovered(
            host_name,
            host_pubkey,
            addr,
            native_window,
            max_video_frames,
        )
        .await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (
            host_name,
            host_pubkey,
            addr,
            native_window,
            max_video_frames,
        );
        Err("play_discovered_host is Android-only".into())
    }
}

/// Pair, start session, and receive up to `max_video_frames` (for M3 stats / soak).
#[flutter_rust_bridge::frb]
pub async fn connect_and_receive(
    payload: String,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let session = connect_as_identity(
        &payload,
        "streambeam-android",
        Some(&client_identity_path()),
    )
    .await
    .map_err(|e| e.to_string())?;

    let _ = save_host_from_payload(payload);

    receive_stats(session, max_video_frames).await
}

/// Decode and render video to an Android Surface (`native_window` = `ANativeWindow*`).
#[flutter_rust_bridge::frb]
pub async fn play_stream(
    payload: String,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        return crate::android::play::run(payload, native_window, max_video_frames).await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (payload, native_window, max_video_frames);
        Err("play_stream is Android-only — on macOS run: ./scripts/visual-test.sh".into())
    }
}

/// Reconnect to the last paired host without rescanning QR (host must still trust this client).
#[flutter_rust_bridge::frb]
pub async fn reconnect_play_stream(
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        let host = load_saved_host(&saved_host_path())
            .map_err(|e| e.to_string())?
            .ok_or("no saved host — scan QR first")?;
        let addr = host.primary_addr().ok_or("saved host has no address")?;
        return crate::android::play::run_known(host, addr, native_window, max_video_frames).await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (native_window, max_video_frames);
        Err("reconnect_play_stream is Android-only".into())
    }
}

async fn receive_stats(
    session: streambeam_core::ClientSession,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let mut stats = StreamStats {
        width: session.width,
        height: session.height,
        fps: session.fps,
        codec: session.codec.clone(),
        video_frames: 0,
        audio_packets: 0,
    };

    let mut asm = FrameReassembler::default();
    let target = max_video_frames.max(1);

    while stats.video_frames < u64::from(target) {
        let datagram = tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram())
            .await
            .map_err(|_| "timed out waiting for media".to_string())?
            .map_err(|e| e.to_string())?;

        let header = MediaHeader::decode(&datagram).map_err(|e| e.to_string())?;
        if header.channel == CHANNEL_AUDIO {
            stats.audio_packets += 1;
            continue;
        }
        if asm
            .push_datagram(&datagram)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            stats.video_frames = asm.stats().frames_complete;
        }
    }

    Ok(stats)
}

// --- Host role (Windows desktop) ---

#[derive(Debug, Clone)]
pub struct HostClientInfo {
    pub name: String,
    pub addr: String,
    pub host_pubkey: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct HostPairRequest {
    pub id: u64,
    pub client_name: String,
    pub client_addr: String,
}

#[derive(Debug, Clone)]
pub struct HostSnapshot {
    pub running: bool,
    pub host_name: String,
    pub listen_addr: String,
    pub pairing_link: Option<String>,
    pub pairing_payload: Option<String>,
    pub error: Option<String>,
    pub discoverable: bool,
    pub accepts_connections: bool,
    pub clients: Vec<HostClientInfo>,
    pub pending_approval: Option<HostPairRequest>,
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_platform_supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "android"))
}

#[flutter_rust_bridge::frb(sync)]
pub fn start_host(
    host_name: Option<String>,
    listen: Option<String>,
    fps: u32,
    max_encode_dim: u32,
) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    {
        return crate::host::start(host_name, listen, fps, max_encode_dim);
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = (host_name, listen, fps, max_encode_dim);
        Err("hosting is only supported on Windows".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn stop_host() {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    crate::host::stop();
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_snapshot() -> HostSnapshot {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    {
        let s = crate::host::snapshot();
        return HostSnapshot {
            running: s.running,
            host_name: s.host_name,
            listen_addr: s.listen_addr,
            pairing_link: s.pairing_link,
            pairing_payload: s.pairing_payload,
            error: s.error,
            discoverable: s.discoverable,
            accepts_connections: s.accepts_connections,
            clients: s
                .clients
                .into_iter()
                .map(|c| HostClientInfo {
                    name: c.name,
                    addr: c.addr,
                    host_pubkey: c.pubkey,
                })
                .collect(),
            pending_approval: s.pending_approval.map(|p| HostPairRequest {
                id: p.id,
                client_name: p.client_name,
                client_addr: p.client_addr,
            }),
        };
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    HostSnapshot {
        running: false,
        host_name: String::new(),
        listen_addr: String::new(),
        pairing_link: None,
        pairing_payload: None,
        error: None,
        discoverable: false,
        accepts_connections: false,
        clients: Vec::new(),
        pending_approval: None,
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_set_discoverable(on: bool) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    return crate::host::set_discoverable(on);
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = on;
        Err("hosting is only supported on Windows".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_set_accepts_connections(on: bool) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    return crate::host::set_accepts_connections(on);
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = on;
        Err("hosting is only supported on Windows".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_disconnect_client(addr: String) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    return crate::host::disconnect_client(&addr);
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = addr;
        Err("hosting is only supported on Windows".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_forget_client(addr: String) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    return crate::host::forget_client(&addr);
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = addr;
        Err("hosting is only supported on Windows".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_respond_pairing(id: u64, allow: bool) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    return crate::host::respond_pairing(id, allow);
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        let _ = (id, allow);
        Err("hosting is only supported on Windows".into())
    }
}
