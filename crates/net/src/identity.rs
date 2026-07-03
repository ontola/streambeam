use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::Result;

/// Long-term Ed25519 identity wrapped in a self-signed X.509 certificate.
pub struct Identity {
    key_pair: KeyPair,
    cert: rcgen::Certificate,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let key_pair = KeyPair::generate_for(&PKCS_ED25519)?;
        let cert = CertificateParams::default().self_signed(&key_pair)?;
        Ok(Self { key_pair, cert })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key_pair
            .public_key_raw()
            .try_into()
            .expect("Ed25519 public key is 32 bytes")
    }

    pub fn cert_der(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert.der().to_vec())
    }

    pub fn private_key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            self.key_pair.serialize_der(),
        ))
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}
