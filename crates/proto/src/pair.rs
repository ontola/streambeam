//! Pairing payload (`sbm1:` QR) and enrollment proof.

use std::net::SocketAddr;

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::ProtoError;

type HmacSha256 = Hmac<Sha256>;

pub const PAIR_PREFIX: &str = "sbm1:";
pub const PAIR_EXPORTER_LABEL: &[u8] = b"streambeam-pair";
pub const PAIR_EXPORTER_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairInfo {
    pub version: u8,
    pub host_name: String,
    pub host_pubkey: [u8; 32],
    pub addrs: Vec<SocketAddr>,
    pub token: [u8; 16],
    pub expires_unix: u64,
}

impl PairInfo {
    pub fn encode_payload(&self) -> Result<String, ProtoError> {
        let bytes = postcard::to_allocvec(self)?;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Ok(format!("{PAIR_PREFIX}{b64}"))
    }

    pub fn decode_payload(s: &str) -> Result<Self, ProtoError> {
        let b64 = s
            .strip_prefix(PAIR_PREFIX)
            .ok_or(ProtoError::BadPairPayload("missing sbm1: prefix"))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64)
            .map_err(|_| ProtoError::BadPairPayload("invalid base64url"))?;
        postcard::from_bytes(&bytes).map_err(Into::into)
    }
}

/// `pair_proof = HMAC-SHA256(token, client_pubkey || tls_exporter(label, 32))`
pub fn compute_pair_proof(
    token: &[u8; 16],
    client_pubkey: &[u8; 32],
    exporter: &[u8; PAIR_EXPORTER_LEN],
) -> [u8; 32] {
    let mut mac =
        HmacSha256::new_from_slice(token).expect("HMAC accepts any key length up to block size");
    mac.update(client_pubkey);
    mac.update(exporter);
    mac.finalize().into_bytes().into()
}

pub fn verify_pair_proof(
    token: &[u8; 16],
    client_pubkey: &[u8; 32],
    exporter: &[u8; PAIR_EXPORTER_LEN],
    proof: &[u8; 32],
) -> bool {
    let expected = compute_pair_proof(token, client_pubkey, exporter);
    expected.ct_eq(proof).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_roundtrip() {
        let info = PairInfo {
            version: 1,
            host_name: "fakehost".into(),
            host_pubkey: [1u8; 32],
            addrs: vec!["127.0.0.1:47989".parse().unwrap()],
            token: [2u8; 16],
            expires_unix: 1_700_000_000,
        };
        let payload = info.encode_payload().unwrap();
        assert!(payload.starts_with(PAIR_PREFIX));
        let decoded = PairInfo::decode_payload(&payload).unwrap();
        assert_eq!(decoded, info);
    }

    #[test]
    fn pair_proof_deterministic() {
        let token = [3u8; 16];
        let pk = [4u8; 32];
        let exp = [5u8; 32];
        let a = compute_pair_proof(&token, &pk, &exp);
        let b = compute_pair_proof(&token, &pk, &exp);
        assert_eq!(a, b);
        assert!(verify_pair_proof(&token, &pk, &exp, &a));
        assert!(!verify_pair_proof(&token, &pk, &exp, &[0u8; 32]));
    }
}
