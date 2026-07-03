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

/// The payload rides in the URL *fragment*: browsers never send fragments to
/// the server, so the one-time token can't leak into streambeam.eu access logs.
pub const PAIR_URL_BASE: &str = "https://streambeam.eu/session/#";

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

    /// URL form for QR codes: opens the app directly when installed (Android
    /// App Link), or the download/fallback page when not.
    pub fn encode_url(&self) -> Result<String, ProtoError> {
        Ok(format!("{PAIR_URL_BASE}{}", self.encode_payload()?))
    }

    /// Strip whitespace and validate before decode (Windows terminal wraps often truncate copies).
    pub fn normalize_payload(s: &str) -> String {
        s.trim()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// Accepts both the raw `sbm1:` form and the QR URL form
    /// (`https://streambeam.eu/session/#sbm1:…`). A raw payload never contains
    /// `#` (base64url alphabet), so splitting on the last `#` is unambiguous.
    pub fn decode_payload(s: &str) -> Result<Self, ProtoError> {
        let s = Self::normalize_payload(s);
        let s = match s.rsplit_once('#') {
            Some((_, fragment)) => fragment.to_owned(),
            None => s,
        };
        let b64 = s
            .strip_prefix(PAIR_PREFIX)
            .ok_or(ProtoError::BadPairPayload(
                "Not a pairing code — it should start with sbm1:",
            ))?;
        if b64.len() < 32 {
            return Err(ProtoError::BadPairPayload(
                "Pairing code looks cut off. Copy the whole sbm1: line from your PC, or scan the QR.",
            ));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64)
            .map_err(|_| {
                ProtoError::BadPairPayload(
                    "Invalid pairing code. Copy sbm1: exactly from streambeam-host on your PC.",
                )
            })?;
        postcard::from_bytes(&bytes).map_err(|e| {
            if matches!(e, postcard::Error::DeserializeUnexpectedEnd) {
                ProtoError::BadPairPayload(
                    "Pairing code incomplete — only part was copied. Scan the QR or copy the full sbm1: line from your PC.",
                )
            } else {
                ProtoError::Postcard(e)
            }
        })
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
    fn payload_ignores_whitespace() {
        let info = PairInfo {
            version: 1,
            host_name: "host".into(),
            host_pubkey: [1u8; 32],
            addrs: vec!["192.168.0.10:47989".parse().unwrap()],
            token: [2u8; 16],
            expires_unix: 1_700_000_000,
        };
        let payload = info.encode_payload().unwrap();
        let wrapped = format!("  {}  ", payload);
        let broken = payload.replace(':', ":\n");
        assert_eq!(PairInfo::decode_payload(&wrapped).unwrap(), info);
        assert_eq!(PairInfo::decode_payload(&broken).unwrap(), info);
    }

    #[test]
    fn truncated_payload_is_clear() {
        let info = PairInfo {
            version: 1,
            host_name: "host".into(),
            host_pubkey: [1u8; 32],
            addrs: vec!["127.0.0.1:47989".parse().unwrap()],
            token: [2u8; 16],
            expires_unix: 1_700_000_000,
        };
        let payload = info.encode_payload().unwrap();
        let truncated = &payload[..payload.len() / 2];
        let err = PairInfo::decode_payload(truncated).unwrap_err().to_string();
        assert!(err.contains("incomplete") || err.contains("cut off"), "{err}");
    }

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
    fn url_form_roundtrip() {
        let info = PairInfo {
            version: 1,
            host_name: "host".into(),
            host_pubkey: [1u8; 32],
            addrs: vec!["192.168.0.10:47989".parse().unwrap()],
            token: [2u8; 16],
            expires_unix: 1_700_000_000,
        };
        let url = info.encode_url().unwrap();
        assert!(url.starts_with("https://streambeam.eu/session/#sbm1:"));
        assert_eq!(PairInfo::decode_payload(&url).unwrap(), info);
        // QR scanners sometimes hand over the URL with stray whitespace.
        assert_eq!(
            PairInfo::decode_payload(&format!(" {url}\n")).unwrap(),
            info
        );
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
