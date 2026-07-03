//! QUIC endpoint, Ed25519 identity, pinned-key mTLS, pairing enrollment.

pub mod config;
pub mod discovery;
pub mod enroll;
pub mod error;
pub mod identity;
pub mod stream;
pub mod verify;

pub use config::{client_config, ensure_crypto_provider, peer_certs, peer_pubkey, server_config};
pub use discovery::{
    advertise, advertise_ips, browse, guess_lan_ip, Advertiser, DiscoveredHost, SERVICE_TYPE,
};
pub use enroll::{
    build_pair_proof, export_pairing_secret, HelloDecision, HelloVerdict, PairedClient,
    PairingStore,
};
pub use error::{NetError, Result};
pub use identity::Identity;
pub use stream::{read_msg, write_msg};
pub use verify::{extract_ed25519_pubkey, PinHostVerifier};
