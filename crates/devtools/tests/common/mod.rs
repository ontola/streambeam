//! Shared helpers for devtools integration tests.
// Each test binary compiles its own copy and uses only some of the helpers.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn bin(name: &str) -> String {
    std::env::var(format!("CARGO_BIN_EXE_{name}"))
        .unwrap_or_else(|_| format!("{}/../../target/debug/{name}", env!("CARGO_MANIFEST_DIR")))
}

fn host_bin() -> String {
    // `streambeam-host` lives in a different crate, so cargo doesn't expose a
    // `CARGO_BIN_EXE_streambeam-host` for it here (that's only set for bins in
    // *this* package, like fakehost/devclient) and `cargo test --workspace`
    // doesn't necessarily build another crate's binary. Since this integration
    // test shells out to it, build it on demand when missing so a plain
    // `cargo test` is self-contained (see AGENTS.md).
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_streambeam-host") {
        return path;
    }
    let path = format!(
        "{}/../../target/debug/streambeam-host",
        env!("CARGO_MANIFEST_DIR")
    );
    if !Path::new(&path).exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "streambeam-host"])
            .status()
            .expect("build streambeam-host");
        assert!(
            status.success(),
            "failed to build streambeam-host for e2e test"
        );
    }
    path
}

pub fn wait_for_payload(reader: &mut BufReader<impl std::io::Read>) -> String {
    let start = std::time::Instant::now();
    let mut line = String::new();
    while start.elapsed() < Duration::from_secs(10) {
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

pub fn spawn_fakehost() -> (std::process::Child, String) {
    spawn_fakehost_with_env(&[])
}

pub fn spawn_fakehost_with_env(extra: &[(&str, &str)]) -> (std::process::Child, String) {
    let mut child = Command::new(bin("fakehost"));
    child
        .env("STREAMBEAM_LISTEN", "127.0.0.1:0")
        .env("STREAMBEAM_NO_MDNS", "1");
    for (k, v) in extra {
        child.env(k, v);
    }
    let mut child = child
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fakehost");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let payload = wait_for_payload(&mut reader);
    (child, payload)
}

pub fn spawn_host(data_dir: &Path) -> (std::process::Child, String) {
    let mut child = Command::new(host_bin())
        .env("STREAMBEAM_LISTEN", "127.0.0.1:0")
        .env("STREAMBEAM_NO_MDNS", "1")
        // The CLI bin has no Swift/consent capture layer; without the mock,
        // macOS sessions wait 30s for ScreenCaptureKit and fail.
        .env("STREAMBEAM_MOCK", "1")
        .env("STREAMBEAM_DATA_DIR", data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn streambeam-host");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let payload = wait_for_payload(&mut reader);
    (child, payload)
}

pub fn run_devclient(
    payload: &str,
    frames: u32,
    drop_pct: u8,
    verify: bool,
) -> std::process::ExitStatus {
    run_devclient_opts(payload, frames, drop_pct, verify, 0, None)
}

pub fn run_devclient_opts(
    payload: &str,
    frames: u32,
    drop_pct: u8,
    verify: bool,
    soak_secs: u64,
    data_dir: Option<&Path>,
) -> std::process::ExitStatus {
    let mut cmd = Command::new(bin("devclient"));
    cmd.arg(payload);
    if soak_secs > 0 {
        cmd.arg("--soak-secs").arg(soak_secs.to_string());
        cmd.arg("--frames").arg("0");
    } else {
        cmd.arg("--frames").arg(frames.to_string());
    }
    if drop_pct > 0 {
        cmd.arg("--drop").arg(drop_pct.to_string());
    }
    if !verify {
        cmd.arg("--no-verify");
    }
    if let Some(dir) = data_dir {
        cmd.env("STREAMBEAM_CLIENT_DATA", dir);
    }
    cmd.status().expect("run devclient")
}

pub fn run_devclient_reconnect(data_dir: &Path, frames: u32) -> std::process::ExitStatus {
    Command::new(bin("devclient"))
        .arg("sbm1:ignored")
        .arg("--reconnect")
        .arg("--frames")
        .arg(frames.to_string())
        .env("STREAMBEAM_CLIENT_DATA", data_dir)
        .status()
        .expect("reconnect devclient")
}
