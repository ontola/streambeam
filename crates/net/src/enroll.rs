use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use streambeam_proto::pair::{compute_pair_proof, verify_pair_proof, PAIR_EXPORTER_LEN};

use crate::error::{NetError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedClient {
    pub pubkey: [u8; 32],
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedPairing {
    paired: Vec<PairedClient>,
}

#[derive(Debug, Clone)]
struct ActiveToken {
    token: [u8; 16],
    expires_unix: u64,
}

/// Host-side pairing state: one-time QR token + enrolled clients.
#[derive(Debug, Default)]
pub struct PairingStore {
    active: Option<ActiveToken>,
    paired: Vec<PairedClient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloDecision {
    AcceptedNewPair,
    AcceptedKnown,
    Rejected(String),
}

impl PairingStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load enrolled clients from `paired.json` (token is always minted fresh per session).
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read_to_string(path)
            .map_err(|e| NetError::Other(format!("read paired store: {e}")))?;
        let file: PersistedPairing = serde_json::from_str(&data)
            .map_err(|e| NetError::Other(format!("parse paired store: {e}")))?;
        Ok(Self {
            active: None,
            paired: file.paired,
        })
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| NetError::Other(format!("create data dir: {e}")))?;
        }
        let file = PersistedPairing {
            paired: self.paired.clone(),
        };
        let data = serde_json::to_string_pretty(&file)
            .map_err(|e| NetError::Other(format!("serialize paired store: {e}")))?;
        std::fs::write(path, data)
            .map_err(|e| NetError::Other(format!("write paired store: {e}")))?;
        Ok(())
    }

    pub fn paired_clients(&self) -> &[PairedClient] {
        &self.paired
    }

    pub fn is_paired(&self, pubkey: &[u8; 32]) -> bool {
        self.paired.iter().any(|c| &c.pubkey == pubkey)
    }

    /// Mint a fresh one-time token valid for `ttl_secs`.
    pub fn mint_token(&mut self, ttl_secs: u64) -> [u8; 16] {
        let mut token = [0u8; 16];
        rand::rng().fill_bytes(&mut token);
        let expires_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs()
            + ttl_secs;
        self.active = Some(ActiveToken {
            token,
            expires_unix,
        });
        token
    }

    pub fn active_token(&self) -> Option<[u8; 16]> {
        self.active.as_ref().and_then(|t| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs();
            if now <= t.expires_unix {
                Some(t.token)
            } else {
                None
            }
        })
    }

    pub fn burn_token(&mut self) {
        self.active = None;
    }

    pub fn handle_hello(
        &mut self,
        client_pubkey: [u8; 32],
        client_name: &str,
        pair_proof: Option<[u8; 32]>,
        exporter: &[u8; PAIR_EXPORTER_LEN],
    ) -> HelloDecision {
        if self.is_paired(&client_pubkey) {
            return HelloDecision::AcceptedKnown;
        }

        let Some(proof) = pair_proof else {
            return HelloDecision::Rejected("unknown client".into());
        };

        let Some(token) = self.active_token() else {
            return HelloDecision::Rejected("pairing token expired".into());
        };

        if !verify_pair_proof(&token, &client_pubkey, exporter, &proof) {
            return HelloDecision::Rejected("invalid pair proof".into());
        }

        self.burn_token();
        self.paired.push(PairedClient {
            pubkey: client_pubkey,
            name: client_name.to_owned(),
        });
        HelloDecision::AcceptedNewPair
    }
}

pub fn export_pairing_secret(conn: &quinn::Connection) -> Result<[u8; PAIR_EXPORTER_LEN]> {
    let mut out = [0u8; PAIR_EXPORTER_LEN];
    conn.export_keying_material(&mut out, streambeam_proto::pair::PAIR_EXPORTER_LABEL, b"")
        .map_err(|e| NetError::Other(format!("tls exporter failed: {e:?}")))?;
    Ok(out)
}

pub fn build_pair_proof(
    token: &[u8; 16],
    client_pubkey: &[u8; 32],
    exporter: &[u8; PAIR_EXPORTER_LEN],
) -> [u8; 32] {
    compute_pair_proof(token, client_pubkey, exporter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_store_roundtrip() {
        let dir = std::env::temp_dir().join("streambeam-test-paired");
        let path = dir.join("paired.json");
        let _ = std::fs::remove_file(&path);

        let mut store = PairingStore::new();
        store.paired.push(PairedClient {
            pubkey: [7u8; 32],
            name: "phone".into(),
        });
        store.save_to(&path).unwrap();

        let loaded = PairingStore::load_from(&path).unwrap();
        assert_eq!(loaded.paired_clients().len(), 1);
        assert_eq!(loaded.paired_clients()[0].name, "phone");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
