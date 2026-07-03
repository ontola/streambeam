use std::path::Path;

use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::{NetError, Result};

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

    /// Load PKCS#8 Ed25519 private key from `path`, or generate and persist a new one.
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        if path.exists() {
            let der = std::fs::read(path)
                .map_err(|e| NetError::Other(format!("read identity {}: {e}", path.display())))?;
            let key_pair = KeyPair::try_from(der.as_slice())
                .map_err(|e| NetError::Other(format!("parse identity key: {e}")))?;
            let cert = CertificateParams::default().self_signed(&key_pair)?;
            return Ok(Self { key_pair, cert });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| NetError::Other(format!("create identity dir: {e}")))?;
        }
        let id = Self::generate()?;
        std::fs::write(path, id.key_pair.serialize_der())
            .map_err(|e| NetError::Other(format!("write identity {}: {e}", path.display())))?;
        Ok(id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_generate_roundtrip() {
        let path = std::env::temp_dir().join("streambeam-id-test.pkcs8");
        let _ = std::fs::remove_file(&path);
        let a = Identity::load_or_generate(&path).unwrap();
        let b = Identity::load_or_generate(&path).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        let _ = std::fs::remove_file(&path);
    }
}
