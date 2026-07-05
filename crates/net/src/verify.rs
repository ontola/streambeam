use std::sync::Arc;

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DistinguishedName, Error, SignatureScheme};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::error::{NetError, Result};

const ED25519_OID: &str = "1.3.101.112";

/// Extract the raw 32-byte Ed25519 public key from an X.509 certificate.
pub fn extract_ed25519_pubkey(cert_der: &[u8]) -> Result<[u8; 32]> {
    let (_, cert) = X509Certificate::from_der(cert_der)
        .map_err(|_| NetError::InvalidCert("failed to parse certificate"))?;
    let spki = cert.public_key();
    if spki.algorithm.algorithm.to_id_string() != ED25519_OID {
        return Err(NetError::InvalidCert("expected Ed25519 certificate"));
    }
    let bits = spki.subject_public_key.data.as_ref();
    let raw: &[u8] = match bits.len() {
        32 => bits,
        33 if bits[0] == 0 => &bits[1..],
        _ => return Err(NetError::InvalidCert("unexpected Ed25519 key length")),
    };
    raw.try_into()
        .map_err(|_| NetError::InvalidCert("expected 32-byte Ed25519 key"))
}

/// Client-side verifier: accept only a pinned host Ed25519 public key.
#[derive(Debug)]
pub struct PinHostVerifier {
    pinned: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl PinHostVerifier {
    pub fn new(pinned: [u8; 32], algs: WebPkiSupportedAlgorithms) -> Self {
        Self { pinned, algs }
    }
}

impl ServerCertVerifier for PinHostVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        let pk = extract_ed25519_pubkey(end_entity.as_ref())
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if pk != self.pinned {
            return Err(Error::InvalidCertificate(CertificateError::UnknownIssuer));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Server-side verifier: require a well-formed client Ed25519 certificate.
/// Authorization happens at the Hello control message.
#[derive(Debug)]
pub struct AcceptClientCertVerifier {
    algs: WebPkiSupportedAlgorithms,
}

impl AcceptClientCertVerifier {
    pub fn new(algs: WebPkiSupportedAlgorithms) -> Arc<Self> {
        Arc::new(Self { algs })
    }
}

impl ClientCertVerifier for AcceptClientCertVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, Error> {
        extract_ed25519_pubkey(end_entity.as_ref())
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}
