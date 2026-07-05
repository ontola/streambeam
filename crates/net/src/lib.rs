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
    Advertiser, Browser, DiscoveredHost, SERVICE_TYPE, advertise, advertise_ips, browse,
    guess_lan_ip,
};
pub use enroll::{
    HelloDecision, HelloVerdict, PairedClient, PairingStore, build_pair_proof,
    export_pairing_secret,
};
pub use error::{NetError, Result};
pub use identity::Identity;
pub use stream::{read_msg, write_msg};
pub use verify::{PinHostVerifier, extract_ed25519_pubkey};
