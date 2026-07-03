//! Headless dev client: pairing + ping/pong against fakehost.

use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use streambeam_proto::control::{ClientMsg, HostMsg};
use streambeam_proto::pair::PairInfo;
use streambeam_proto::PROTO_VERSION;
use streambeam_net::{
    build_pair_proof, client_config, export_pairing_secret, read_msg, write_msg, Identity,
};
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(name = "devclient")]
struct Opt {
    /// Pairing payload from fakehost (`sbm1:...`).
    payload: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let opt = Opt::parse();
    if let Err(e) = run(&opt.payload).await {
        error!("{e}");
        std::process::exit(1);
    }
}

async fn run(payload: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let info = PairInfo::decode_payload(payload)?;
    if info.version != PROTO_VERSION {
        return Err(format!("unsupported pair version {}", info.version).into());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now > info.expires_unix {
        return Err("pairing payload expired".into());
    }

    let addr = info
        .addrs
        .first()
        .copied()
        .ok_or("pairing payload has no addresses")?;

    let identity = Identity::generate()?;
    let mut client = quinn::Endpoint::client("[::]:0".parse()?)?;
    client.set_default_client_config(client_config(&identity, info.host_pubkey)?);

    info!(%addr, host = %info.host_name, "connecting");
    let conn = client.connect(addr, "localhost")?.await?;
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
            client_name: "devclient".into(),
            pair_proof,
        },
    )
    .await?;

    let mut buf = Vec::new();
    match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
        HostMsg::PairAck => info!("paired"),
        HostMsg::PairReject { reason } => return Err(format!("pair rejected: {reason}").into()),
        other => return Err(format!("unexpected host reply: {other:?}").into()),
    }

    let t_us = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
    write_msg(&mut send, &ClientMsg::Ping { t_us }).await?;
    match read_msg::<HostMsg>(&mut recv, &mut buf).await? {
        HostMsg::Pong { t_us: echoed } if echoed == t_us => info!(t_us, "pong ok"),
        other => return Err(format!("unexpected pong: {other:?}").into()),
    }

    write_msg(&mut send, &ClientMsg::Bye).await?;
    let _ = read_msg::<HostMsg>(&mut recv, &mut buf).await;
    conn.close(0u32.into(), b"bye");
    client.wait_idle().await;
    info!("done");
    Ok(())
}
