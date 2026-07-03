//! One-command demo: start fakehost and open playclient.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin(name: &str) -> String {
    std::env::var(format!("CARGO_BIN_EXE_{name}")).unwrap_or_else(|_| {
        format!(
            "{}/../../target/debug/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    })
}

fn main() {
    eprintln!("starting fakehost…");
    let mut host = Command::new(bin("fakehost"))
        .env("STREAMBEAM_LISTEN", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn fakehost");

    let mut reader = BufReader::new(host.stdout.take().expect("stdout"));
    let payload = {
        let start = std::time::Instant::now();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).ok() == Some(0) {
                if start.elapsed() > Duration::from_secs(5) {
                    panic!("timed out waiting for sbm1: payload");
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if line.trim().starts_with("sbm1:") {
                break line.trim().to_string();
            }
        }
    };

    eprintln!("launching playclient (video window should appear in ~1s)…");
    let status = Command::new(bin("playclient"))
        .arg(&payload)
        .status()
        .expect("playclient");

    let _ = host.kill();
    let _ = host.wait();

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
