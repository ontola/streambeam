//! Stream integrity: hash verify and loss injection against fakehost.

mod common;

use common::{run_devclient, spawn_fakehost};

#[test]
fn sixty_frames_hash_ok() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient(&payload, 60, 0, true);
    assert!(status.success(), "expected 60/60 hash verify");
    let _ = child.kill();
}

#[test]
fn loss_sweep_five_percent() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient(&payload, 60, 5, true);
    assert!(status.success(), "FEC should recover at 5% drop");
    let _ = child.kill();
}

#[test]
fn loss_sweep_zero_percent() {
    let (mut child, payload) = spawn_fakehost();
    let status = run_devclient(&payload, 30, 0, true);
    assert!(status.success());
    let _ = child.kill();
}
