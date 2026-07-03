//! Fake host: speaks the real protocol for the macOS dev loop.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streambeam_proto::control::{ClientMsg, HostMsg};
use streambeam_proto::pair::PairInfo;
use streambeam_proto::PROTO_VERSION;
use streambeam_net::{
    export_pairing_secret, peer_pubkey, read_msg, server_config, write_msg, HelloDecision,
    Identity, PairingStore,
};
use tokio::signal;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let listen: SocketAddr = std::env::var("STREAMBEAM_LISTEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:47989".parse().expect("addr"));

    let identity = Identity::generate().expect("identity");
    let store = Arc::new(Mutex::new(PairingStore::new()));
    let token = store.lock().unwrap().mint_token(300);
    let expires_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 300;

    let endpoint = quinn::Endpoint::server(server_config(&identity).unwrap(), listen)
        .expect("bind");
    let addr = endpoint.local_addr().expect("local addr");

    let payload = PairInfo {
        version: 1,
        host_name: "fakehost".into(),
        host_pubkey: identity.public_key(),
        addrs: vec![addr],
        token,
        expires_unix,
    }
    .encode_payload()
    .expect("payload");

    println!("{payload}");
    info!(%addr, "fakehost listening — scan or paste payload into devclient");

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let store = store.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(incoming, store).await {
                        error!("connection failed: {e}");
                    }
                });
            }
            _ = signal::ctrl_c() => {
                info!("shutting down");
                break;
            }
        }
    }

    endpoint.wait_idle().await;
}

async fn handle(
    incoming: quinn::Incoming,
    store: Arc<Mutex<PairingStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = incoming.await?;
    info!(remote = %conn.remote_address(), "connected");

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
    let decision = store.lock().unwrap().handle_hello(
        client_pk,
        &client_name,
        pair_proof,
        &exporter,
    );

    match decision {
        HelloDecision::AcceptedNewPair => {
            info!(%client_name, "paired new client");
            write_msg(&mut send, &HostMsg::PairAck).await?;
        }
        HelloDecision::AcceptedKnown => {
            info!(%client_name, "known client");
            write_msg(&mut send, &HostMsg::PairAck).await?;
        }
        HelloDecision::Rejected(reason) => {
            info!(%client_name, %reason, "pair rejected");
            write_msg(&mut send, &HostMsg::PairReject { reason }).await?;
            return Ok(());
        }
    }

    // M0 demo: ping/pong on the same control stream.
    loop {
        let msg: ClientMsg = match tokio::time::timeout(Duration::from_secs(30), read_msg(&mut recv, &mut buf)).await {
            Ok(Ok(m)) => m,
            Ok(Err(_)) | Err(_) => break,
        };
        match msg {
            ClientMsg::Ping { t_us } => {
                write_msg(&mut send, &HostMsg::Pong { t_us }).await?;
            }
            ClientMsg::Bye => {
                write_msg(&mut send, &HostMsg::Bye).await?;
                break;
            }
            other => {
                info!(?other, "ignored client message");
            }
        }
    }

    conn.close(0u32.into(), b"bye");
    Ok(())
}
