//! Pairing enrollment logic (no QUIC I/O).

use streambeam_net::{HelloDecision, HelloVerdict, PairingStore, build_pair_proof};
use streambeam_proto::pair::PAIR_EXPORTER_LEN;

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
fn enroll_allows_unknown_without_proof_for_lan_approval() {
    // No proof presented: evaluate_hello still surfaces a NeedsApproval
    // verdict so an *interactive* host UI can prompt the user, but it must
    // be flagged as unverified — see `handle_hello_rejects_headless_without_proof`
    // for the headless (no UI) counterpart, which must reject this case.
    let store = PairingStore::new();
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    assert_eq!(
        store.evaluate_hello(client_pk, None, &exporter),
        HelloVerdict::NeedsApproval {
            proof_verified: false
        }
    );
}

#[test]
fn handle_hello_rejects_headless_without_proof() {
    // Regression test for the auto-enrollment bypass: a headless host (using
    // handle_hello directly, with no interactive approval UI) must not
    // enroll a client that presents no pairing proof, even though
    // evaluate_hello reports NeedsApproval for that same input.
    let mut store = PairingStore::new();
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    assert!(matches!(
        store.handle_hello(client_pk, "phone", None, &exporter),
        HelloDecision::Rejected(_)
    ));
    assert!(!store.is_paired(&client_pk));
}

#[test]
fn handle_hello_accepts_verified_proof_for_new_client() {
    // The headless (QR-scanned) path: a client presenting a valid proof
    // against the active token is a verified NeedsApproval and handle_hello
    // may enroll it directly.
    let mut store = PairingStore::new();
    let token = store.mint_token(300);
    let client_pk = [9u8; 32];
    let exporter = [1u8; PAIR_EXPORTER_LEN];
    let proof = build_pair_proof(&token, &client_pk, &exporter);
    assert_eq!(
        store.evaluate_hello(client_pk, Some(proof), &exporter),
        HelloVerdict::NeedsApproval {
            proof_verified: true
        }
    );
    assert_eq!(
        store.handle_hello(client_pk, "phone", Some(proof), &exporter),
        HelloDecision::AcceptedNewPair
    );
    assert!(store.is_paired(&client_pk));
}

#[test]
fn evaluate_defers_enrollment_until_commit() {
    // The host UI gates a new device on user approval: evaluate_hello must
    // report the valid device without enrolling it, and only commit_new_pair
    // (called after the user clicks Allow) may enroll and burn the token.
    let mut store = PairingStore::new();
    let token = store.mint_token(300);
    let client_pk = [4u8; 32];
    let exporter = [5u8; PAIR_EXPORTER_LEN];
    let proof = build_pair_proof(&token, &client_pk, &exporter);

    assert_eq!(
        store.evaluate_hello(client_pk, Some(proof), &exporter),
        HelloVerdict::NeedsApproval {
            proof_verified: true
        }
    );
    // Read-only: repeated evaluation is stable and nothing is enrolled yet.
    assert_eq!(
        store.evaluate_hello(client_pk, Some(proof), &exporter),
        HelloVerdict::NeedsApproval {
            proof_verified: true
        }
    );
    assert!(!store.is_paired(&client_pk));
    assert!(store.active_token().is_some());

    // Simulate the user clicking Allow.
    store.commit_new_pair(client_pk, "phone");
    assert!(store.is_paired(&client_pk));
    assert!(
        store.active_token().is_none(),
        "one-time token must be burned"
    );

    // A denied device (no commit) is simply never enrolled — nothing to assert
    // beyond the store staying unchanged for other pubkeys.
    assert!(!store.is_paired(&[7u8; 32]));
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
