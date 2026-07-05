//! Stream integrity: hash verify, loss sweep, soak, and reconnect.

mod common;

use std::path::PathBuf;

use common::{
    run_devclient, run_devclient_opts, run_devclient_reconnect, spawn_fakehost, spawn_host,
};

#[test]
fn sixty_frames_hash_ok() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient(&payload, 60, 0, true);
    assert!(status.success(), "expected 60/60 hash verify");
    let _ = child.kill();
}

#[test]
fn loss_sweep_zero_percent() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient(&payload, 30, 0, true);
    assert!(status.success());
    let _ = child.kill();
}

#[test]
fn loss_sweep_up_to_fifteen_percent() {
    for drop_pct in [1u8, 2, 3, 5, 8, 10, 12, 15] {
        let (mut child, payload) = spawn_fakehost();
        let status = run_devclient(&payload, 60, drop_pct, true);
        assert!(status.success(), "FEC should recover at {drop_pct}% drop");
        let _ = child.kill();
    }
}

#[test]
fn short_soak_eight_seconds() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient_opts(&payload, 0, 0, true, 8, None);
    assert!(status.success(), "8s soak should receive frames");
    let _ = child.kill();
}

/// Full 10-minute soak — run manually: `cargo test -p streambeam-devtools long_soak -- --ignored`
#[test]
#[ignore = "10-minute endurance test; run explicitly before release"]
fn long_soak_ten_minutes() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient_opts(&payload, 0, 0, true, 600, None);
    assert!(status.success());
    let _ = child.kill();
}

#[test]
fn known_client_reconnect_without_qr() {
    let dir = std::env::temp_dir().join("streambeam-reconnect-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (mut host, payload) = spawn_host(&dir);
    let status = run_devclient_opts(&payload, 30, 0, true, 0, Some(&dir));
    assert!(status.success(), "initial pair failed");

    // Host still running with paired.json + stable identity; token in payload is stale.
    let status = run_devclient_reconnect(&dir, 30);
    assert!(status.success(), "reconnect without fresh QR should work");

    let _ = host.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
