//! End-to-end pairing demo: fakehost + devclient subprocesses.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use streambeam_proto::pair::PairInfo;

fn bin(name: &str) -> String {
    std::env::var(format!("CARGO_BIN_EXE_{name}")).unwrap_or_else(|_| {
        format!(
            "{}/../../target/debug/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    })
}

fn wait_for_payload(reader: &mut BufReader<impl std::io::Read>) -> String {
    let start = std::time::Instant::now();
    let mut line = String::new();
    while start.elapsed() < Duration::from_secs(5) {
        line.clear();
        if reader.read_line(&mut line).ok() == Some(0) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if line.trim().starts_with("sbm1:") {
            return line.trim().to_string();
        }
    }
    panic!("timed out waiting for fakehost pairing payload");
}

fn spawn_fakehost() -> (std::process::Child, String) {
    let mut child = Command::new(bin("fakehost"))
        .env("STREAMBEAM_LISTEN", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fakehost");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let payload = wait_for_payload(&mut reader);
    (child, payload)
}

#[test]
fn pair_success() {
    let (mut child, payload) = spawn_fakehost();
    let status = Command::new(bin("devclient"))
        .arg(&payload)
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
