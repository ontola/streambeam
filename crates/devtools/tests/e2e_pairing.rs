//! End-to-end pairing demo: fakehost + devclient subprocesses.

mod common;

use common::{bin, spawn_fakehost};
use streambeam_proto::pair::PairInfo;
use std::process::Command;

#[test]
fn pair_success() {
    let (mut child, payload) = spawn_fakehost();
    let status = Command::new(bin("devclient"))
        .arg(&payload)
        .arg("--frames")
        .arg("10")
        .arg("--no-verify")
        .status()
        .expect("run devclient");
    assert!(status.success(), "devclient failed");
    let _ = child.kill();
}

#[test]
fn wrong_token_rejected() {
    let (mut child, payload) = spawn_fakehost();
    let mut info = PairInfo::decode_payload(&payload).expect("decode payload");
    info.token = [0xAA; 16];
    let bad = info.encode_payload().expect("encode payload");

    let status = Command::new(bin("devclient"))
        .arg(&bad)
        .status()
        .expect("run devclient");
    assert!(!status.success(), "devclient should fail with bad token");
    let _ = child.kill();
}

#[test]
fn unknown_key_rejected() {
    let (mut child, payload) = spawn_fakehost();

    let status1 = Command::new(bin("devclient"))
        .arg(&payload)
        .arg("--frames")
        .arg("5")
        .arg("--no-verify")
        .status()
        .expect("first pair");
    assert!(status1.success());

    let status2 = Command::new(bin("devclient"))
        .arg(&payload)
        .status()
        .expect("second pair");
    assert!(!status2.success(), "unknown client should be rejected");
    let _ = child.kill();
}
