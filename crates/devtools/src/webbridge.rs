//! Opt-in, single-viewer H.264 bridge. Native protocol/authentication stay unchanged.
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use streambeam_core::{FrameOrder, FrameReassembler, access_unit_is_keyframe};
use streambeam_proto::media::{CHANNEL_VIDEO, MediaHeader};
use subtle::ConstantTimeEq;
use tokio::time::timeout;
use url::Url;
use wtransport::{Endpoint, Identity, ServerConfig, VarInt};

type Error = Box<dyn std::error::Error + Send + Sync>;
const MAX_FRAME: usize = 4 * 1024 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const INVITE_TTL: Duration = Duration::from_secs(300);

#[derive(Parser)]
#[command(about = "Experimental video-only browser bridge; not a production host")]
struct Opt {
    /// File containing the native host's pairing payload (never put it in arguments).
    #[arg(long)]
    pair_file: PathBuf,
    /// New private file for the browser invitation URL; must not already exist.
    #[arg(long)]
    invite_file: PathBuf,
    /// Origin hosting site/view/. HTTPS required except for loopback development.
    #[arg(long, default_value = "http://127.0.0.1:8876/view/")]
    viewer_url: Url,
    /// Explicit IP to expose the encrypted gateway on; loopback by default.
    #[arg(long, default_value = "127.0.0.1:4433")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() {
    // Deliberately no tracing subscriber: HTTP metadata and invitations are private.
    if let Err(error) = run(Opt::parse()).await {
        eprintln!("webbridge: {error}");
        std::process::exit(1);
    }
}

fn viewer_origin(url: &Url) -> Result<String, Error> {
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "viewer URL must be HTTPS (or localhost HTTP), without credentials, query or fragment"
                .into(),
        );
    }
    Ok(url.origin().ascii_serialization())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn authorized(expected: &[u8; 32], received: &[u8; 32], age: Duration) -> bool {
    bool::from(expected.ct_eq(received)) && age < INVITE_TTL
}

fn private_invitation(path: &PathBuf, invitation: &str) -> Result<(), Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "{invitation}")?;
    Ok(())
}

async fn run(mut opt: Opt) -> Result<(), Error> {
    let origin = viewer_origin(&opt.viewer_url)?;
    if opt.listen.ip().is_unspecified() {
        return Err("use an explicit local IP, not a wildcard address".into());
    }
    if !opt.listen.ip().is_loopback() && opt.viewer_url.scheme() != "https" {
        return Err("a LAN gateway requires an HTTPS viewer origin".into());
    }
    let payload = std::fs::read_to_string(&opt.pair_file)?;
    if payload.len() > 16_384 {
        return Err("pairing file is too large".into());
    }
    // Validate locally before writing an invitation; do not contact the host yet.
    streambeam_proto::pair::PairInfo::decode_payload(payload.trim())?;
    let identity = Identity::self_signed([opt.listen.ip().to_string()])?;
    let hash = identity.certificate_chain().as_slice()[0].hash();
    let fingerprint = hex(hash.as_ref());
    // Limit unauthenticated ingress as well as the application's media queue.
    // HTTP/3 needs its own control/QPACK streams in addition to our auth stream.
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(4u32.into());
    transport.max_concurrent_uni_streams(8u32.into());
    transport.stream_receive_window(65_536u32.into());
    transport.receive_window(262_144u32.into());
    transport.send_window((MAX_FRAME * 2) as u64);
    transport.datagram_receive_buffer_size(Some(65_536));
    let mut config = ServerConfig::builder()
        .with_bind_address(opt.listen)
        .with_custom_transport(identity, transport)
        .build();
    config.quic_config_mut().max_incoming(4);
    config.quic_config_mut().incoming_buffer_size_total(131_072);
    let server = Endpoint::server(config)?;
    let addr = server.local_addr()?;
    let capability: [u8; 32] = rand::random();
    let fragment = format!(
        "v=1&endpoint=https://{addr}/view&cert={fingerprint}&token={}",
        hex(&capability)
    );
    opt.viewer_url.set_fragment(Some(&fragment));
    private_invitation(&opt.invite_file, opt.viewer_url.as_str())?;
    eprintln!("Browser invitation saved privately; expires in 5 minutes. Video only, one viewer.");
    let started = Instant::now();
    let serving = async {
        // Sequential handshakes bound active connections; no detached tasks or media queues.
        loop {
            let remaining = INVITE_TTL.saturating_sub(started.elapsed());
            let incoming = timeout(remaining, server.accept()).await?;
            let request = match timeout(AUTH_TIMEOUT, incoming).await {
                Ok(Ok(request)) => request,
                _ => continue,
            };
            if request.path() != "/view" || request.origin() != Some(origin.as_str()) {
                request.forbidden().await;
                continue;
            }
            let connection = match timeout(AUTH_TIMEOUT, request.accept()).await {
                Ok(Ok(connection)) => connection,
                _ => continue,
            };
            let authentication = timeout(AUTH_TIMEOUT, async {
                let (send, mut recv) = connection.accept_bi().await?;
                let mut received = [0; 32];
                recv.read_exact(&mut received).await?;
                if !authorized(&capability, &received, started.elapsed()) {
                    return Err::<_, Error>("invalid or expired invitation".into());
                }
                Ok(send)
            })
            .await;
            let send = match authentication {
                Ok(Ok(send)) => send,
                _ => {
                    connection.close(VarInt::from_u32(1), b"Authentication failed");
                    continue;
                }
            };
            // One accepted browser consumes this invitation. Only now authenticate upstream.
            let result = relay(payload.trim(), &connection, send).await;
            connection.close(VarInt::from_u32(0), b"Stream ended");
            return result;
        }
    };
    let result = tokio::select! {
        result = serving => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    let _ = std::fs::remove_file(&opt.invite_file);
    result
}

async fn packet(send: &mut wtransport::SendStream, data: &[u8]) -> Result<(), Error> {
    if data.len() > MAX_FRAME {
        return Err("frame exceeds browser limit".into());
    }
    // Slow or suspended viewers terminate instead of accumulating stale video.
    timeout(Duration::from_secs(2), async {
        send.write_all(&(data.len() as u32).to_be_bytes()).await?;
        send.write_all(data).await
    })
    .await??;
    Ok(())
}

async fn relay(
    payload: &str,
    connection: &wtransport::Connection,
    mut send: wtransport::SendStream,
) -> Result<(), Error> {
    let mut session = tokio::select! {
        result = timeout(Duration::from_secs(60), streambeam_core::connect_as(payload, "StreamBeam browser bridge")) => result??,
        _ = connection.closed() => return Ok(()),
    };
    let result = async {
        if session.codec != "h264" {
            return Err::<(), Error>("browser preview requires H.264".into());
        }
        let config = session.codec_config.as_deref().ok_or("missing H.264 configuration")?;
        let (sps, _) = streambeam_core::extract_avc_decoder_config(config).ok_or("invalid H.264 configuration")?;
        if sps.len() < 4 { return Err("short H.264 SPS".into()); }
        let config = config.to_vec();
        let metadata = serde_json::json!({"version":1,"codec":format!("avc1.{}", hex(&sps[1..4])),"width":session.width,"height":session.height,"fps":session.fps});
        packet(&mut send, &serde_json::to_vec(&metadata)?).await?;
        streambeam_core::request_idr(&mut session).await?;
        let mut assembler = FrameReassembler::default();
        let mut order = FrameOrder::default();
        let started = Instant::now();
        let mut waiting_keyframe = true;
        let mut last_idr = Instant::now();
        let mut frame_number = 0u64;
        loop {
            let datagram = tokio::select! {
                result = timeout(Duration::from_secs(10), session.conn.read_datagram()) => result??,
                _ = connection.closed() => return Ok(()),
            };
            let header = MediaHeader::decode(&datagram)?;
            if header.channel != CHANNEL_VIDEO { continue; }
            if let Some((id, frame)) = assembler.push_datagram(&datagram)? {
                order.push(id, frame);
            }
            if assembler.take_loss_report().is_some() { waiting_keyframe = true; }
            for (gap, frame) in order.drain(started.elapsed()) {
                if gap { waiting_keyframe = true; }
                let keyframe = access_unit_is_keyframe(&frame);
                if waiting_keyframe && !keyframe { continue; }
                waiting_keyframe = false;
                let mut data = Vec::with_capacity(frame.len() + config.len() + 9);
                data.push(u8::from(keyframe));
                data.extend_from_slice(&(frame_number * 1_000_000 / u64::from(session.fps.max(1))).to_be_bytes());
                if keyframe { data.extend_from_slice(&config); }
                data.extend_from_slice(&frame);
                packet(&mut send, &data).await?;
                frame_number += 1;
            }
            if waiting_keyframe && last_idr.elapsed() >= Duration::from_secs(1) {
                streambeam_core::request_idr(&mut session).await?;
                last_idr = Instant::now();
            }
        }
    }.await;
    session
        .conn
        .close(quinn::VarInt::from_u32(0), b"Browser viewer closed");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_are_exact_and_expire() {
        assert!(authorized(&[7; 32], &[7; 32], Duration::ZERO));
        assert!(!authorized(&[7; 32], &[8; 32], Duration::ZERO));
        assert!(!authorized(&[7; 32], &[7; 32], INVITE_TTL));
    }
    #[test]
    fn viewer_origin_rejects_insecure_remote_and_secret_urls() {
        for value in [
            "http://192.168.0.4/view/",
            "https://user:pass@example.com/view/",
            "https://example.com/view/?token=x",
            "https://example.com/view/#token",
        ] {
            assert!(viewer_origin(&Url::parse(value).unwrap()).is_err());
        }
        assert_eq!(
            viewer_origin(&Url::parse("http://127.0.0.1:8876/view/").unwrap()).unwrap(),
            "http://127.0.0.1:8876"
        );
        assert!(viewer_origin(&Url::parse("https://streambeam.eu/view/").unwrap()).is_ok());
    }
}
