//! Pairing + session setup shared by devtools and mobile FFI.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use streambeam_net::{
    build_pair_proof, client_config, export_pairing_secret, read_msg, write_msg, Identity,
};
use streambeam_proto::control::{AudioCaps, ClientMsg, HostMsg, VideoCaps};
use streambeam_proto::pair::PairInfo;
use streambeam_proto::PROTO_VERSION;

#[derive(Debug)]
pub struct ClientSession {
    pub _endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: String,
    pub codec_config: Option<Vec<u8>>,
}

pub async fn connect(payload: &str) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    connect_as(payload, "streambeam-client").await
}

pub async fn connect_as(
    payload: &str,
    client_name: &str,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    connect_as_identity(payload, client_name, None).await
}

/// Like [`connect_as`] but persists the client Ed25519 identity at `identity_path` when set.
pub async fn connect_as_identity(
    payload: &str,
    client_name: &str,
    identity_path: Option<&Path>,
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
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(client_config(&identity, info.host_pubkey)?);

    let conn = endpoint
        .connect(addr, "localhost")?
        .await
        .map_err(|e| format!("connect failed (is host running?): {e}"))?;

    let exporter = export_pairing_secret(&conn)?;
    let pair_proof = Some(build_pair_proof(
        &info.token,
        &identity.public_key(),
        &exporter,
    ));

    finish_session(
        endpoint,
        conn,
        client_name,
        pair_proof,
    )
    .await
}

/// Reconnect to a previously paired host (no QR token required).
pub async fn connect_known(
    host_pubkey: [u8; 32],
    addr: SocketAddr,
    client_name: &str,
    identity_path: &Path,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    let identity = load_client_identity(Some(identity_path))?;
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(client_config(&identity, host_pubkey)?);

    let conn = endpoint
        .connect(addr, "localhost")?
        .await
        .map_err(|e| format!("connect failed (is host running?): {e}"))?;

    finish_session(endpoint, conn, client_name, None).await
}

fn load_client_identity(path: Option<&Path>) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    match path {
        Some(p) => Ok(Identity::load_or_generate(p)?),
        None => Ok(Identity::generate()?),
    }
}

async fn finish_session(
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    client_name: &str,
    pair_proof: Option<[u8; 32]>,
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

    let mut codec_config = None;
    let (width, height, fps, codec) = loop {
        match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
            HostMsg::SessionAccept {
                width,
                height,
                fps,
                codec,
                ..
            } => break (width, height, fps, codec),
            HostMsg::CodecConfig { data } => {
                codec_config = Some(data);
            }
            other => return Err(format!("expected SessionAccept, got {other:?}").into()),
        }
    };

    Ok(ClientSession {
        _endpoint: endpoint,
        conn,
        send,
        recv,
        width,
        height,
        fps,
        codec,
        codec_config,
    })
}

/// Ask the host to send an IDR (keyframe) on the next video frame.
pub async fn request_idr(
    session: &mut ClientSession,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_msg(&mut session.send, &ClientMsg::RequestIdr).await?;
    Ok(())
}
