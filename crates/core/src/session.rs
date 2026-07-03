//! Pairing + session setup shared by devtools and mobile FFI.

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
}

pub async fn connect(payload: &str) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    connect_as(payload, "streambeam-client").await
}

pub async fn connect_as(
    payload: &str,
    client_name: &str,
) -> Result<ClientSession, Box<dyn std::error::Error + Send + Sync>> {
    let info = PairInfo::decode_payload(payload)?;
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
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(client_config(&identity, info.host_pubkey)?);

    let conn = endpoint
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
            client_name: client_name.into(),
            pair_proof,
        },
    )
    .await?;

    let mut buf = Vec::new();
    match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
        HostMsg::PairAck => {}
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

    let (width, height, fps, codec) = loop {
        match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
            HostMsg::SessionAccept {
                width,
                height,
                fps,
                codec,
                ..
            } => break (width, height, fps, codec),
            HostMsg::CodecConfig { .. } => {}
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
    })
}
