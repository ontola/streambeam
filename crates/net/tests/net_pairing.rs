//! Pairing enrollment logic (no QUIC I/O).

use streambeam_proto::pair::PAIR_EXPORTER_LEN;
use streambeam_net::{build_pair_proof, HelloDecision, PairingStore};

#[test]
fn enroll_accepts_valid_proof() {
    let mut store = PairingStore::new();
    let token = store.mint_token(300);
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    let proof = build_pair_proof(&token, &client_pk, &exporter);
    assert_eq!(
        store.handle_hello(client_pk, "phone", Some(proof), &exporter),
        HelloDecision::AcceptedNewPair
    );
    assert!(store.is_paired(&client_pk));
}

#[test]
fn enroll_rejects_bad_token() {
    let mut store = PairingStore::new();
    let _token = store.mint_token(300);
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    let proof = build_pair_proof(&[0u8; 16], &client_pk, &exporter);
    assert!(matches!(
        store.handle_hello(client_pk, "phone", Some(proof), &exporter),
        HelloDecision::Rejected(_)
    ));
}

#[test]
fn enroll_rejects_unknown_without_proof() {
    let mut store = PairingStore::new();
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    assert!(matches!(
        store.handle_hello(client_pk, "phone", None, &exporter),
        HelloDecision::Rejected(_)
    ));
}

#[test]
fn known_client_skips_proof() {
    let mut store = PairingStore::new();
    let token = store.mint_token(300);
    let client_pk = [2u8; 32];
    let exporter = [3u8; PAIR_EXPORTER_LEN];
    let proof = build_pair_proof(&token, &client_pk, &exporter);
    store.handle_hello(client_pk, "phone", Some(proof), &exporter);
    assert_eq!(
        store.handle_hello(client_pk, "phone", None, &exporter),
        HelloDecision::AcceptedKnown
    );
}
