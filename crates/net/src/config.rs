use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::crypto::ring::default_provider;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls_pki_types::CertificateDer;

use streambeam_proto::ALPN;

use crate::error::{NetError, Result};
use crate::identity::Identity;
use crate::verify::{AcceptClientCertVerifier, PinHostVerifier};

fn supported_algs() -> WebPkiSupportedAlgorithms {
    WebPkiSupportedAlgorithms::from(default_provider().signature_verification_algorithms)
}

pub fn ensure_crypto_provider() {
    let _ = default_provider().install_default();
}

pub fn server_config(identity: &Identity) -> Result<quinn::ServerConfig> {
    ensure_crypto_provider();
    let algs = supported_algs();
    let rustls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(AcceptClientCertVerifier::new(algs))
        .with_single_cert(
            vec![identity.cert_der()],
            identity.private_key_der(),
        )?;
    let mut rustls = rustls;
    rustls.alpn_protocols = vec![ALPN.to_vec()];

    let mut server =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(rustls)?));
    let transport = Arc::get_mut(&mut server.transport).expect("fresh server config");
    transport.max_concurrent_bidi_streams(4_u8.into());
    transport.datagram_send_buffer_size(4 * 1024 * 1024);
    transport.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    Ok(server)
}

pub fn client_config(identity: &Identity, pinned_host: [u8; 32]) -> Result<quinn::ClientConfig> {
    ensure_crypto_provider();
    let algs = supported_algs();
    let verifier = Arc::new(PinHostVerifier::new(pinned_host, algs.clone()));
    let rustls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(
            vec![identity.cert_der()],
            identity.private_key_der(),
        )?;
    let mut rustls = rustls;
    rustls.alpn_protocols = vec![ALPN.to_vec()];

    let mut client =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(rustls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    client.transport_config(Arc::new(transport));
    Ok(client)
}

/// Peer certificate chain from a QUIC connection (rustls backend).
pub fn peer_certs(conn: &quinn::Connection) -> Result<Vec<CertificateDer<'static>>> {
    let identity = conn
        .peer_identity()
        .ok_or(NetError::MissingPeerCert)?;
    identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map(|c| *c)
        .map_err(|_| NetError::MissingPeerCert)
}

pub fn peer_pubkey(conn: &quinn::Connection) -> Result<[u8; 32]> {
    let certs = peer_certs(conn)?;
    let end_entity = certs
        .first()
        .ok_or(NetError::MissingPeerCert)?;
    crate::verify::extract_ed25519_pubkey(end_entity.as_ref())
}
