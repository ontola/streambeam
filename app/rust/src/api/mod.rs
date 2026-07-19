use std::time::Duration;

use streambeam_core::{
    FrameReassembler, SavedHost, clear_saved_host, connect_as_identity, load_saved_host,
    save_saved_host,
};
use streambeam_proto::media::{CHANNEL_AUDIO, MEDIA_HEADER_LEN, MediaHeader};

use crate::store::{client_identity_path, saved_host_path};

#[flutter_rust_bridge::frb(sync)]
pub fn playback_frames_rendered() -> u64 {
    crate::playback::frames_rendered()
}

/// Player sidebar "Sound" toggle: mute/unmute received stream audio.
#[flutter_rust_bridge::frb(sync)]
pub fn set_audio_muted(muted: bool) {
    crate::audio_play::set_muted(muted);
}

/// Stop the active watch session immediately instead of waiting for the
/// decoder/media receive loop's network timeout to notice that its UI left.
#[flutter_rust_bridge::frb(sync)]
pub fn stop_playback() {
    crate::playback::stop_active_connection();
}

#[derive(Debug, Clone)]
pub struct PlaybackDiagnostics {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: String,
    pub bytes_received: u64,
    pub frames_rendered: u64,
    /// QUIC's smoothed round-trip time. One-way transport is estimated as
    /// half this value; this does not include host processing or decode.
    pub network_rtt_us: u64,
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
        network_rtt_us: crate::playback::network_rtt_us(),
    }
}

#[derive(Debug, Clone)]
pub struct PlaybackDisplay {
    pub id: u32,
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub primary: bool,
}

#[flutter_rust_bridge::frb(sync)]
pub fn playback_displays() -> Vec<PlaybackDisplay> {
    crate::playback::displays()
        .into_iter()
        .map(|display| PlaybackDisplay {
            id: display.id,
            name: display.name,
            width: display.width,
            height: display.height,
            primary: display.primary,
        })
        .collect()
}

#[flutter_rust_bridge::frb(sync)]
pub fn select_playback_display(id: u32) {
    crate::playback::request_display(id);
}

#[flutter_rust_bridge::frb(sync)]
pub fn selected_playback_display() -> Option<u32> {
    crate::playback::requested_display_id()
}

// The send_* input functions are platform-generic: they enqueue on the
// active session's input pump (`crate::input`) and silently no-op when no
// session is streaming.

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_move(dx: i16, dy: i16) {
    crate::input::send(streambeam_proto::input::InputEvent::MouseMove { dx, dy });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_touch_frame(ids: Vec<u8>, xs: Vec<u16>, ys: Vec<u16>, ref_w: u16, ref_h: u16) {
    use streambeam_proto::input::{InputEvent, TouchContact};
    if ids.len() != xs.len() || ids.len() != ys.len() || ids.len() > 10 {
        return;
    }
    let contacts = ids
        .into_iter()
        .zip(xs)
        .zip(ys)
        .map(|((id, x), y)| TouchContact {
            id,
            x,
            y,
            pressure: u16::MAX,
        })
        .collect();
    crate::input::send(InputEvent::TouchFrame {
        contacts,
        ref_w,
        ref_h,
    });
}

#[flutter_rust_bridge::frb(sync)]
pub fn playback_host_accepts_touch() -> bool {
    crate::playback::host_accepts_touch()
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_scroll(dx: i16, dy: i16) {
    crate::input::send(streambeam_proto::input::InputEvent::Scroll { dx, dy });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_clipboard_text(text: String) {
    if text.len() <= 60_000 {
        crate::input::send(streambeam_proto::input::InputEvent::ClipboardText { text });
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn take_remote_clipboard() -> Option<String> {
    crate::playback::take_remote_clipboard()
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_abs(x: u16, y: u16, ref_w: u16, ref_h: u16) {
    crate::input::send(streambeam_proto::input::InputEvent::MouseAbs { x, y, ref_w, ref_h });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_click(button: u8) {
    send_mouse_button(button, true);
    send_mouse_button(button, false);
}

/// Separate press/release — needed for drag & text selection with a real
/// mouse (desktop watch); [`send_mouse_click`] stays for tap semantics.
#[flutter_rust_bridge::frb(sync)]
pub fn send_mouse_button(button: u8, down: bool) {
    crate::input::send(streambeam_proto::input::InputEvent::MouseButton { button, down });
}

/// Forward a key press/release. `vk` is a Windows virtual-key code — the
/// wire's canonical key encoding (see PROTOCOL.md); viewers map their
/// platform's key events to VK before sending.
#[flutter_rust_bridge::frb(sync)]
pub fn send_key(vk: u16, down: bool) {
    crate::input::send(streambeam_proto::input::InputEvent::Key { vk, down });
}

#[flutter_rust_bridge::frb(sync)]
pub fn send_gamepad_plug(pad: u8, connected: bool) {
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
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    streambeam_host::logging::install_panic_hook();
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

/// Set the name peers see for this device (client name when watching, host name
/// when beaming). Call at startup with the platform device name.
#[flutter_rust_bridge::frb(sync)]
pub fn set_device_name(name: String) {
    crate::store::set_device_name(&name);
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

// One `ServiceDaemon` for the app's whole lifetime, not a fresh one per poll.
// `streambeam_net::browse` cold-starts mDNS resolution from nothing on every
// call — fine for a one-shot CLI lookup, but this UI polls every few
// seconds, and a response landing a beat late (mDNS responders jitter
// replies) made a host that's perfectly fine look "offline" for that cycle.
// A persistent `Browser` instead keeps a continuously-updated cache: once a
// host has been seen, it stays known until a ServiceRemoved event actually
// says otherwise.
static BROWSER: std::sync::OnceLock<Result<streambeam_net::Browser, String>> =
    std::sync::OnceLock::new();

#[flutter_rust_bridge::frb]
pub async fn discover_hosts(timeout_ms: u32) -> Result<Vec<DiscoveredHost>, String> {
    use std::time::Duration;

    let browser = BROWSER
        .get_or_init(|| streambeam_net::Browser::new().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| e.clone())?;

    let mut hosts = browser.snapshot();
    if hosts.is_empty() {
        // Give a just-created browser (or a host that only just came online)
        // a moment to hear a response before reporting nothing — subsequent
        // polls return whatever's cached immediately, no re-probing needed.
        let grace = Duration::from_millis(u64::from(timeout_ms.max(1000)).min(3000));
        tokio::time::sleep(grace).await;
        hosts = browser.snapshot();
    }
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
/// `max_width`/`max_height`/`max_fps` should be this device's real display
/// capability (Dart reads it via `dart:ui`'s `Display` API — see
/// `stream_player.dart`) so the host can encode at a resolution/frame rate
/// that's actually worth sending, not a fixed guess.
#[flutter_rust_bridge::frb]
pub async fn play_discovered_host(
    host_name: String,
    host_pubkey: Vec<u8>,
    addr: String,
    native_window: i64,
    max_video_frames: u32,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        return crate::android::play::run_discovered(
            host_name,
            host_pubkey,
            addr,
            native_window,
            max_video_frames,
            max_width,
            max_height,
            max_fps,
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
            max_width,
            max_height,
            max_fps,
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
        &crate::store::client_name(),
        Some(&client_identity_path()),
        streambeam_core::fallback_video_caps(),
    )
    .await
    .map_err(|e| e.to_string())?;

    let _ = save_host_from_payload(payload);

    receive_stats(session, max_video_frames).await
}

/// Decode and render video to an Android Surface (`native_window` = `ANativeWindow*`).
/// See `play_discovered_host`'s doc for `max_width`/`max_height`/`max_fps`.
#[flutter_rust_bridge::frb]
pub async fn play_stream(
    payload: String,
    native_window: i64,
    max_video_frames: u32,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        return crate::android::play::run(
            payload,
            native_window,
            max_video_frames,
            max_width,
            max_height,
            max_fps,
        )
        .await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (
            payload,
            native_window,
            max_video_frames,
            max_width,
            max_height,
            max_fps,
        );
        Err("play_stream is Android-only — on macOS run: ./scripts/visual-test.sh".into())
    }
}

/// Reconnect to the last paired host without rescanning QR (host must still trust this client).
/// See `play_discovered_host`'s doc for `max_width`/`max_height`/`max_fps`.
#[flutter_rust_bridge::frb]
pub async fn reconnect_play_stream(
    native_window: i64,
    max_video_frames: u32,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> Result<StreamStats, String> {
    #[cfg(target_os = "android")]
    {
        let host = load_saved_host(&saved_host_path())
            .map_err(|e| e.to_string())?
            .ok_or("no saved host — scan QR first")?;
        let addr = host.primary_addr().ok_or("saved host has no address")?;
        return crate::android::play::run_known(
            host,
            addr,
            native_window,
            max_video_frames,
            max_width,
            max_height,
            max_fps,
        )
        .await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (
            native_window,
            max_video_frames,
            max_width,
            max_height,
            max_fps,
        );
        Err("reconnect_play_stream is Android-only".into())
    }
}

/// One event on the desktop watch stream. The first event is the config
/// (`is_config = true`, geometry + SPS/PPS); every later event is a frame
/// (`is_config = false`, `data` = one Annex-B access unit). A plain struct
/// (not an enum) so the Dart side needs no `freezed` codegen.
#[derive(Debug, Clone)]
pub struct DesktopStreamEvent {
    pub is_config: bool,
    /// Negotiated codec id on the config event: `"h264"` (codec_config =
    /// Annex-B SPS/PPS) or `"av01"` (codec_config = ISO-BMFF `av1C` record,
    /// ready for the platform decoder's format description).
    pub codec: String,
    pub codec_config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fps: u8,
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Desktop watch: connect, reassemble the media plane (reusing `crates/core`),
/// and stream H.264 access units to the platform decoder (VideoToolbox on
/// macOS). Video-only for now; input/control is a separate path.
///
/// Errors go through `sink.add_error` (→ the Dart stream's `onError`), never
/// the function's own `Result`: the generated binding fires that future
/// `unawaited`, so a returned `Err` becomes an unhandled Dart exception the
/// UI never sees.
#[flutter_rust_bridge::frb]
pub async fn play_stream_desktop(
    payload: String,
    allow_av1: bool,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
    sink: crate::frb_generated::StreamSink<DesktopStreamEvent>,
) -> Result<(), String> {
    if let Err(e) = play_stream_desktop_inner(
        payload,
        desktop_video_caps(allow_av1, max_width, max_height, max_fps),
        &sink,
    )
    .await
    {
        let _ = sink.add_error(e);
    }
    Ok(())
}

/// `allow_av1` comes from the platform decoder probe (VideoToolbox hardware
/// AV1 exists on Apple silicon M3+ only) — advertising it blindly would make
/// the host encode a stream the client can't decode. `max_*` are the real
/// display capability from Dart's `Display` query (0 → safe fallback), same
/// contract as the Android path's `video_caps`.
fn desktop_video_caps(
    allow_av1: bool,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> streambeam_proto::control::VideoCaps {
    let fallback = streambeam_core::fallback_video_caps();
    streambeam_proto::control::VideoCaps {
        h264: true,
        hevc: false,
        av1: allow_av1,
        max_width: if max_width > 0 {
            max_width
        } else {
            fallback.max_width
        },
        max_height: if max_height > 0 {
            max_height
        } else {
            fallback.max_height
        },
        max_fps: if max_fps > 0 {
            max_fps
        } else {
            fallback.max_fps
        },
        display_id: crate::playback::requested_display_id(),
    }
}

async fn play_stream_desktop_inner(
    payload: String,
    caps: streambeam_proto::control::VideoCaps,
    sink: &crate::frb_generated::StreamSink<DesktopStreamEvent>,
) -> Result<(), String> {
    let session = connect_as_identity(
        &payload,
        &crate::store::client_name(),
        Some(&client_identity_path()),
        caps,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = save_host_from_payload(payload);
    desktop_stream_loop(session, sink).await
}

/// Like [`play_stream_desktop`] but for a discovered/saved host (no QR token —
/// uses the pinned key; the host prompts to approve on first contact).
/// Errors go through the sink — see [`play_stream_desktop`].
#[flutter_rust_bridge::frb]
pub async fn play_stream_desktop_known(
    host_name: String,
    host_pubkey: Vec<u8>,
    addr: String,
    allow_av1: bool,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
    sink: crate::frb_generated::StreamSink<DesktopStreamEvent>,
) -> Result<(), String> {
    if let Err(e) = play_stream_desktop_known_inner(
        host_name,
        host_pubkey,
        addr,
        desktop_video_caps(allow_av1, max_width, max_height, max_fps),
        &sink,
    )
    .await
    {
        let _ = sink.add_error(e);
    }
    Ok(())
}

async fn play_stream_desktop_known_inner(
    host_name: String,
    host_pubkey: Vec<u8>,
    addr: String,
    caps: streambeam_proto::control::VideoCaps,
    sink: &crate::frb_generated::StreamSink<DesktopStreamEvent>,
) -> Result<(), String> {
    let pubkey: [u8; 32] = host_pubkey
        .try_into()
        .map_err(|_| "host public key must be 32 bytes".to_string())?;
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| format!("invalid host address: {e}"))?;
    let session = streambeam_core::connect_known(
        pubkey,
        addr,
        &crate::store::client_name(),
        &client_identity_path(),
        caps,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = save_saved_host(
        &saved_host_path(),
        &SavedHost {
            host_name,
            host_pubkey: pubkey,
            addrs: vec![addr],
        },
    );
    desktop_stream_loop(session, sink).await
}

/// Shared desktop watch loop: reassemble the media plane and stream H.264
/// access units to the platform decoder.
async fn desktop_stream_loop(
    mut session: streambeam_core::ClientSession,
    sink: &crate::frb_generated::StreamSink<DesktopStreamEvent>,
) -> Result<(), String> {
    let _active_connection = crate::playback::ActiveConnectionGuard::new(&session.conn);
    crate::playback::set_host_accepts_touch(matches!(
        session.input_mode,
        streambeam_proto::control::HostInputMode::Touch
    ));
    use streambeam_proto::media::CHANNEL_VIDEO;

    // Feed the shared playback counters so `playback_diagnostics()` /
    // `playback_frames_rendered()` work identically to the Android path
    // (the player UI polls them regardless of platform).
    crate::playback::reset();
    crate::playback::set_displays(session.displays.clone(), session.selected_display_id);
    crate::playback::set_session(
        session.width,
        session.height,
        session.fps,
        session.codec.clone(),
    );

    // Wire mouse/gamepad control: pump `crate::input::send` onto the
    // session's input stream, torn down whichever way this function exits.
    struct InputPumpGuard(tokio::task::JoinHandle<()>);
    impl Drop for InputPumpGuard {
        fn drop(&mut self) {
            crate::input::set_sender(None);
            self.0.abort();
        }
    }
    let _input_guard = session.input.take().map(|input_send| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        crate::input::set_sender(Some(tx));
        InputPumpGuard(tokio::spawn(crate::input::pump(input_send, rx)))
    });
    let _control_guard = session.recv.take().map(|recv| {
        InputPumpGuard(tokio::spawn(crate::playback::receive_control(
            recv,
            session.conn.clone(),
        )))
    });

    let codec_config = session
        .codec_config
        .clone()
        .ok_or("Host did not send video configuration (CodecConfig).")?;
    let is_av1 = session.codec == "av01";
    // In-band reconfiguration (H.264): hosts that can change geometry
    // mid-session (a rotating phone) prepend SPS/PPS to every keyframe; a
    // keyframe whose parameter sets differ from the current ones means the
    // stream size changed and the platform decoder must be rebuilt. Track
    // the active parameter sets to compare against.
    let mut current_sps_pps = if is_av1 {
        None
    } else {
        streambeam_core::extract_avc_decoder_config(&codec_config)
    };
    // H.264 config passes through as Annex-B SPS/PPS; for AV1 the platform
    // decoder wants an `av1C` record, built here from the sequence header in
    // the host's config (same extraction the Android decoder does).
    let codec_config = if is_av1 {
        let seq = streambeam_core::av1_extract_sequence_header(&codec_config)
            .ok_or("Host AV1 config is missing a sequence header.")?;
        streambeam_core::av1c_from_sequence_header(&seq)
            .ok_or("Could not parse the host's AV1 sequence header.")?
    } else {
        codec_config
    };
    sink.add(DesktopStreamEvent {
        is_config: true,
        codec: session.codec.clone(),
        codec_config,
        width: u32::from(session.width),
        height: u32::from(session.height),
        fps: session.fps,
        data: Vec::new(),
        keyframe: false,
    })
    .map_err(|e| e.to_string())?;

    // Ask for a keyframe up front so decoding can start immediately.
    let _ = streambeam_core::request_idr(&mut session).await;

    let mut asm = FrameReassembler::default();
    let audio = crate::audio_play::AudioPlayback::start();
    // A forced IDR or ref-frame invalidation costs the host a live NVENC
    // reconfigure (see `crates/host/src/capture.rs`/`nvenc.rs` — no longer a
    // full session recreate, but still real work and, per real-device
    // testing, still worth not spamming), which itself briefly disrupts the
    // stream and can cause the very next loss report this loop would react
    // to, otherwise turning one real loss event into a self-sustaining
    // recovery storm (matches the Android client's own cooldown — see
    // `IDR_REQUEST_COOLDOWN` in `app/rust/src/android/play.rs`).
    const RECOVERY_COOLDOWN: Duration = Duration::from_secs(3);
    let mut last_recovery = None::<tokio::time::Instant>;
    // Latched the moment a non-keyframe completion is skipped (see the
    // behind-skip below): every subsequent P-frame references frames the
    // decoder was never fed, so feeding them "decodes" into progressively
    // drifting garbage rather than erroring. Once set, nothing but a
    // keyframe is fed — and an IDR is requested (cooldown permitting) so
    // the blank-out is bounded by round-trip + encode, not by the GOP.
    // Root-caused 2026-07-18: the skip used to apply per drain-batch only,
    // so one "behind" burst (keyframe delivery, a UI stall) broke the
    // reference chain and then resumed feeding broken P-frames anyway —
    // the persistent purple-smear corruption on the macOS viewer, on a
    // bitstream a reference decoder replayed bit-clean.
    let mut awaiting_keyframe = false;
    loop {
        let first =
            match tokio::time::timeout(Duration::from_secs(10), session.conn.read_datagram()).await
            {
                Ok(Ok(datagram)) => datagram,
                Ok(Err(_)) if crate::playback::remote_stop_requested() => break,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => return Err("timed out waiting for media".to_string()),
            };
        crate::playback::note_network_rtt(session.conn.stats().path.rtt);

        // Greedily grab anything already sitting in the receive queue too —
        // more than one datagram back-to-back means decode fell behind real
        // time since the last iteration (same reasoning as Android's batch
        // read in `play.rs`).
        let mut batch = vec![first];
        while let Ok(Ok(next)) =
            tokio::time::timeout(Duration::from_millis(2), session.conn.read_datagram()).await
        {
            batch.push(next);
        }

        let mut completions: Vec<(bool, Vec<u8>)> = Vec::new();
        for datagram in batch {
            crate::playback::note_bytes_received(datagram.len() as u64);
            let header = MediaHeader::decode(&datagram).map_err(|e| e.to_string())?;
            if header.channel != CHANNEL_VIDEO {
                if header.channel == CHANNEL_AUDIO && datagram.len() > MEDIA_HEADER_LEN {
                    audio.push(&datagram[MEDIA_HEADER_LEN..]);
                }
                continue;
            }
            if let Some((_id, frame)) = asm.push_datagram(&datagram).map_err(|e| e.to_string())? {
                completions.push((header.is_keyframe(), frame));
            }
        }

        // Several frames finishing in one greedy-drain window is common on a
        // fast LAN at high fps (and right after a host-side NVENC session
        // recreate, which briefly stalls then flushes a small burst) —
        // treating that as "behind" would self-inflict resync requests with
        // zero real loss. A real, growing backlog keeps piling up far more
        // than a short burst, so require a much larger one to call it in.
        // Android's play.rs settled on a flat 5 after real-device testing,
        // but that was tuned for a stream capped at 60fps — at double the
        // frame rate, healthy bursts routinely land twice as many
        // completions in the same 2ms drain window, so a flat threshold
        // false-triggers "behind" on completely normal delivery. Scale it
        // with the session's actual fps, floored at Android's original
        // tuned value so a slower stream doesn't get *more* sensitive than
        // what was proven reliable there.
        // Raised from `fps*5/60` (10 at 120fps) after the awaiting-keyframe
        // latch landed: at an honest 120fps delivery cadence, a single brief
        // app stall (~100ms of Flutter main-thread jank) piles up 12+
        // completions in one drain — tripping "behind" turned that harmless
        // hiccup into reference-chain surgery plus an IDR request every
        // cooldown tick, observed live 2026-07-19 as a 262KB banded intra
        // flash and an fps sawtooth every 3s. A quarter-second of genuine
        // backlog is the point where skipping starts beating decoding.
        let behind_threshold = (u32::from(session.fps) / 4).max(10);
        let behind = completions.len() as u32 > behind_threshold;
        // Skip-mode for THIS batch: entered by a backlog or a still-armed
        // `awaiting_keyframe`, and — critically — *exited by the first
        // keyframe in the batch*, feeding everything after it. The previous
        // shape re-checked `behind` per completion, so the P-frames right
        // behind a recovery keyframe (which arrive in the same large drain
        // — the host's forced-IDR recreate stalls ~400ms, guaranteeing a
        // "behind" batch) re-armed the latch that keyframe had just
        // cleared: one rendered frame per IDR request, a ~5s slideshow,
        // reported live 2026-07-19 as "no flashes but about 0fps".
        let mut resync = awaiting_keyframe || behind;
        let mut dart_closed = false;

        for (keyframe, frame) in completions {
            // Always decode a keyframe (resets the reference chain and is
            // worth showing even a little late). Otherwise, while behind,
            // skip every non-keyframe completion — and once even one
            // P-frame is skipped, keep skipping (`awaiting_keyframe`) until
            // a keyframe actually restores the reference chain: feeding a
            // P-frame whose references were skipped decodes "successfully"
            // into garbage rather than erroring, indistinguishable from
            // corrupted video (same failure mode Android's play.rs
            // documents for MediaCodec), and it stays garbage for the rest
            // of the GOP.
            if keyframe {
                resync = false;
                awaiting_keyframe = false;
            } else if resync {
                awaiting_keyframe = true;
                continue;
            }
            // "Rendered" here means handed to the platform decoder — the
            // closest observable point on desktop, where decode happens in
            // the native layer past this crate.
            crate::playback::note_frame_rendered();
            // Same per-frame cleanup as the Android AV1 path — some decoders
            // choke on the redundant frame headers NVENC emits.
            let data = if is_av1 {
                streambeam_core::av1_access_unit_without_redundant_headers(&frame)
            } else {
                frame
            };
            // Rotation / mid-session resize: new parameter sets on a keyframe
            // → tell Dart to rebuild the decoder before feeding it this frame.
            // Dimensions are left 0 — the platform decoder reads them from
            // the new SPS and reports them back from `configure`.
            if !is_av1 && keyframe {
                if let Some(new_cfg) = streambeam_core::extract_avc_decoder_config(&data) {
                    if current_sps_pps.as_ref() != Some(&new_cfg) {
                        let mut cfg = streambeam_core::nal_with_start_code(&new_cfg.0);
                        cfg.extend_from_slice(&streambeam_core::nal_with_start_code(&new_cfg.1));
                        current_sps_pps = Some(new_cfg);
                        if sink
                            .add(DesktopStreamEvent {
                                is_config: true,
                                codec: session.codec.clone(),
                                codec_config: cfg,
                                width: 0,
                                height: 0,
                                fps: session.fps,
                                data: Vec::new(),
                                keyframe: false,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            if sink
                .add(DesktopStreamEvent {
                    is_config: false,
                    codec: String::new(),
                    codec_config: Vec::new(),
                    width: 0,
                    height: 0,
                    fps: 0,
                    data,
                    keyframe,
                })
                .is_err()
            {
                // Dart closed the stream (playback stopped).
                dart_closed = true;
                break;
            }
        }
        if dart_closed {
            break;
        }

        // Recover from unrecoverable loss the same way the other clients do
        // — sharing one cooldown clock with the skip-driven resync below so
        // neither can spam the host even if both trip in the same tick.
        let cooled_down = last_recovery
            .map(|t| t.elapsed() >= RECOVERY_COOLDOWN)
            .unwrap_or(true);
        if cooled_down && let Some(report) = asm.take_loss_report() {
            last_recovery = Some(tokio::time::Instant::now());
            match report {
                streambeam_core::LossReport::Invalidate {
                    first_frame,
                    last_frame,
                } => {
                    let _ = streambeam_core::request_invalidate_ref_frames(
                        &mut session,
                        first_frame,
                        last_frame,
                    )
                    .await;
                }
                streambeam_core::LossReport::NeedIdr => {
                    let _ = streambeam_core::request_idr(&mut session).await;
                }
            }
        } else if cooled_down && awaiting_keyframe {
            // Skip-driven resync: we're deliberately feeding nothing until a
            // keyframe (see `awaiting_keyframe`), so make one arrive sooner
            // than the periodic GOP keyframe would.
            last_recovery = Some(tokio::time::Instant::now());
            let _ = streambeam_core::request_idr(&mut session).await;
        }
    }

    let _ = session.conn.close(0u32.into(), b"bye");
    Ok(())
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
    /// Authenticated clients currently receiving this device's media stream.
    pub viewers: Vec<HostClientInfo>,
    pub pending_approval: Option<HostPairRequest>,
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_platform_supported() -> bool {
    cfg!(any(
        target_os = "windows",
        target_os = "android",
        target_os = "macos"
    ))
}

/// Public key of the host currently running in this app. The Watch screen uses
/// this stable identity to hide our own mDNS advertisement without relying on
/// potentially-colliding device names or IP addresses.
#[flutter_rust_bridge::frb(sync)]
pub fn host_public_key() -> Vec<u8> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    {
        return crate::host::public_key();
    }
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    Vec::new()
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_publish_clipboard(text: String) {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    if text.len() <= 60_000 {
        crate::host::publish_clipboard(text);
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_take_remote_clipboard() -> Option<String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    {
        return crate::host::take_remote_clipboard();
    }
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    None
}

#[flutter_rust_bridge::frb(sync)]
pub fn start_host(
    host_name: Option<String>,
    listen: Option<String>,
    fps: u32,
    max_encode_dim: u32,
) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    {
        return crate::host::start(host_name, listen, fps, max_encode_dim);
    }
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = (host_name, listen, fps, max_encode_dim);
        Err("hosting is not supported on this platform".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn stop_host() {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    crate::host::stop();
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_snapshot() -> HostSnapshot {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
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
            viewers: s
                .viewers
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
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
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
        viewers: Vec::new(),
        pending_approval: None,
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_set_discoverable(on: bool) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    return crate::host::set_discoverable(on);
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = on;
        Err("hosting is not supported on this platform".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_set_accepts_connections(on: bool) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    return crate::host::set_accepts_connections(on);
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = on;
        Err("hosting is not supported on this platform".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_disconnect_client(addr: String) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    return crate::host::disconnect_client(&addr);
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = addr;
        Err("hosting is not supported on this platform".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_forget_client(addr: String) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    return crate::host::forget_client(&addr);
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = addr;
        Err("hosting is not supported on this platform".into())
    }
}

#[flutter_rust_bridge::frb(sync)]
pub fn host_respond_pairing(id: u64, allow: bool) -> Result<bool, String> {
    #[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
    return crate::host::respond_pairing(id, allow);
    #[cfg(not(any(target_os = "windows", target_os = "android", target_os = "macos")))]
    {
        let _ = (id, allow);
        Err("hosting is not supported on this platform".into())
    }
}
