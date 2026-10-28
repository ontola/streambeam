//! Pairing + session setup shared by devtools and mobile FFI.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use streambeam_net::{
    Identity, build_pair_proof, client_config, export_pairing_secret, read_msg, write_msg,
};
use streambeam_proto::PROTO_VERSION;
use streambeam_proto::control::{
    AudioCaps, AudioParams, ClientMsg, HostInputMode, HostMsg, VideoCaps,
};
use streambeam_proto::input::InputEvent;
use streambeam_proto::pair::PairInfo;

#[derive(Debug)]
pub struct ClientSession {
    pub _endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: Option<quinn::RecvStream>,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: String,
    pub codec_config: Option<Vec<u8>>,
    pub displays: Vec<streambeam_proto::control::DisplayInfo>,
    pub selected_display_id: u32,
    pub input_mode: HostInputMode,
    pub audio: AudioParams,
    pub input: Option<quinn::SendStream>,
}

/// Turn a QUIC handshake failure into something a user can actually act on,
/// instead of one generic "connect failed (is host running?)" bucket that
/// looked identical whether the host was genuinely off, a firewall silently
/// dropped every packet, or the two sides just disagreed on something. Each
/// of these has a different fix, so they should say different things.
fn describe_connect_failure(e: &quinn::ConnectionError) -> String {
    use quinn::ConnectionError;
    match e {
        // No response at all within the idle timeout — the single most
        // common real-world cause isn't "host is off", it's a firewall (or a
        // network profile change re-scoping an existing firewall rule —
        // see docs/PROGRESS.md's 2026-07-07 entry) silently dropping every
        // packet with no rejection, which looks identical to the host not
        // running from the client's point of view.
        ConnectionError::TimedOut => {
            "No response from the host after several seconds. Check that streambeam-host is \
             running, both devices are on the same network, and — if this worked before but \
             stopped after a network change — that Windows Firewall still allows it on the \
             current network profile (Private/Public)."
                .to_string()
        }
        ConnectionError::VersionMismatch => {
            "Host and client QUIC versions don't match — update the app.".to_string()
        }
        ConnectionError::Reset => {
            "Host reset the connection (it may have just restarted). Try again.".to_string()
        }
        ConnectionError::ApplicationClosed(reason) => {
            format!("Host closed the connection: {reason}")
        }
        other => format!("Connection failed: {other}"),
    }
}

/// A generous, technically-safe fallback for callers (devtools, tests) that
/// don't have a real device to ask — actual clients should pass their own
/// display's real capability instead (see `VideoCaps`'s doc in
/// `streambeam_proto::control`).
pub fn fallback_video_caps() -> VideoCaps {
    VideoCaps {
        h264: true,
        hevc: false,
        av1: false,
        max_width: 1920,
        max_height: 1080,
        max_fps: 60,
        display_id: None,
        // Off unless a client positively knows it can decode *and* display
        // HDR10; a wrong true here is a garbled picture, a wrong false is
        // just today's tone-mapped SDR.
        hdr: false,
    }
}

pub async fn connect(
    payload: &str,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    connect_as(payload, "streambeam-client").await
}

pub async fn connect_as(
    payload: &str,
    client_name: &str,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    connect_as_identity(payload, client_name, None, fallback_video_caps()).await
}

/// Like [`connect_as`] but persists the client Ed25519 identity at `identity_path` when set.
/// `video_caps` should reflect the connecting device's real display — see
/// [`fallback_video_caps`] for callers with no real device to ask.
pub async fn connect_as_identity(
    payload: &str,
    client_name: &str,
    identity_path: Option<&Path>,
    video_caps: VideoCaps,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    let info = PairInfo::decode_payload(payload)?;
    if info.version != PROTO_VERSION {
        return Err(format!("unsupported pair version {}", info.version).into());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now > info.expires_unix {
        return Err(
            "pairing payload expired — rescan host QR or use Reconnect if already paired".into(),
        );
    }

    let addr = info
        .addrs
        .first()
        .copied()
        .ok_or("pairing payload has no addresses")?;

    let identity = load_client_identity(identity_path)?;
    let (endpoint, conn) = dial(&identity, info.host_pubkey, addr).await?;

    let exporter = export_pairing_secret(&conn)?;
    let pair_proof = Some(build_pair_proof(
        &info.token,
        &identity.public_key(),
        &exporter,
    ));

    finish_session(endpoint, conn, client_name, pair_proof, video_caps).await
}

/// Reconnect to a previously paired host (no QR token required). `video_caps`
/// should reflect the connecting device's real display — see
/// [`fallback_video_caps`] for callers with no real device to ask.
pub async fn connect_known(
    host_pubkey: [u8; 32],
    addr: SocketAddr,
    client_name: &str,
    identity_path: &Path,
    video_caps: VideoCaps,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    let identity = load_client_identity(Some(identity_path))?;
    let (endpoint, conn) = dial(&identity, host_pubkey, addr).await?;

    finish_session(endpoint, conn, client_name, None, video_caps).await
}

/// Open a client endpoint pinned to `host_pubkey` and complete the QUIC
/// handshake with `addr`. The local socket binds the unspecified address of
/// the target's family, so an IPv4 host stays reachable on machines without
/// IPv6 support (where binding `[::]` fails outright).
async fn dial(
    identity: &Identity,
    host_pubkey: [u8; 32],
    addr: SocketAddr,
) -> Result<(quinn::Endpoint, quinn::Connection), Box<dyn std::error::Error + Send + Sync>> {
    let bind: SocketAddr = match addr {
        SocketAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(client_config(identity, host_pubkey)?);

    let conn = endpoint
        .connect(addr, "localhost")?
        .await
        .map_err(|e| describe_connect_failure(&e))?;
    Ok((endpoint, conn))
}

fn load_client_identity(
    path: Option<&Path>,
) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    match path {
        Some(p) => Ok(Identity::load_or_generate(p)?),
        None => Ok(Identity::generate()?),
    }
}

/// The QUIC connection stays alive on keep-alives even when the host never
/// answers the handshake (wedged encoder, incompatible/older host build) —
/// without a deadline the client would show "Connecting…" forever.
// A first client-initiated macOS session may need the host user to approve the
// Screen Recording prompt after pairing. Keep a finite deadline, but allow
// enough time for that one-time OS consent interaction.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn finish_session(
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    client_name: &str,
    pair_proof: Option<[u8; 32]>,
    video_caps: VideoCaps,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        finish_session_inner(endpoint, conn, client_name, pair_proof, video_caps),
    )
    .await
    .map_err(|_| {
        "host accepted the connection but did not complete the session handshake — \
         it may be busy starting its encoder or running an incompatible (older) \
         version; restart or update the host app"
    })?
}

async fn finish_session_inner(
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    client_name: &str,
    pair_proof: Option<[u8; 32]>,
    video_caps: VideoCaps,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(
        &mut send,
        &ClientMsg::Hello {
            proto_version: PROTO_VERSION,
            client_name: client_name.into(),
            pair_proof,
        },
    )
    .await?;

    let mut buf = Vec::new();
    match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
        HostMsg::PairAck => {}
        HostMsg::PairReject { reason } => {
            return Err(format!("pair rejected: {reason}").into());
        }
        other => return Err(format!("unexpected host reply: {other:?}").into()),
    }

    write_msg(
        &mut send,
        &ClientMsg::SessionRequest {
            video: video_caps,
            audio: AudioCaps {
                opus: true,
                max_channels: 2,
            },
            bitrate_kbps: 20_000,
        },
    )
    .await?;

    let mut codec_config = None;
    let mut displays = Vec::new();
    let mut selected_display_id = 0;
    let (width, height, fps, codec, input_mode, audio) = loop {
        match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
            HostMsg::SessionAccept {
                width,
                height,
                fps,
                codec,
                input_mode,
                audio,
                ..
            } => break (width, height, fps, codec, input_mode, audio),
            HostMsg::CodecConfig { data } => {
                codec_config = Some(data);
            }
            HostMsg::DisplayList {
                displays: available,
                selected_id,
            } => {
                displays = available;
                selected_display_id = selected_id;
            }
            other => return Err(format!("expected SessionAccept, got {other:?}").into()),
        }
    };

    let input = conn.open_uni().await?;

    Ok(ClientSession {
        _endpoint: endpoint,
        conn,
        send,
        recv: Some(recv),
        width,
        height,
        fps,
        codec,
        codec_config,
        displays,
        selected_display_id,
        input_mode,
        audio,
        input: Some(input),
    })
}

impl Drop for ClientSession {
    fn drop(&mut self) {
        // Close the QUIC session so the host drops stale "connected client" entries
        // immediately instead of waiting for a control-stream read timeout.
        self.conn.close(0u32.into(), b"client session ended");
    }
}

/// Send an input event on stream S1 (touch, keys, gamepad).
pub async fn send_input(
    session: &mut ClientSession,
    event: InputEvent,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let input = session.input.as_mut().ok_or("input stream not open")?;
    write_msg(input, &event).await?;
    Ok(())
}

/// Ask the host to send an IDR (keyframe) on the next video frame.
pub async fn request_idr(
    session: &mut ClientSession,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_msg(&mut session.send, &ClientMsg::RequestIdr).await?;
    Ok(())
}

/// Ask the host to suspend (sleep/hibernate). The host acks best-effort and
/// then goes down — expect the connection to die right after; that is
/// success, not an error.
pub async fn request_power(
    session: &mut ClientSession,
    action: streambeam_proto::control::PowerAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_msg(&mut session.send, &ClientMsg::RequestPower { action }).await?;
    Ok(())
}

/// Reference-Frame-Invalidation (RFI) recovery: tell the host it lost video
/// `frame_id`s `[first_frame, last_frame]` (inclusive, wrapping) so the next
/// encoded frame can skip them as references — cheaper than `request_idr`
/// when there's still a usable reference just before this range. See
/// `crate::reassemble::LossReport` for how a caller decides between the two.
pub async fn request_invalidate_ref_frames(
    session: &mut ClientSession,
    first_frame: u16,
    last_frame: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_msg(
        &mut session.send,
        &ClientMsg::InvalidateRefFrames {
            first_frame,
            last_frame,
        },
    )
    .await?;
    Ok(())
}
