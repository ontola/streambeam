//! Graphical client: H.264 + Opus playback via ffplay.

mod client;
mod play;

use std::time::Duration;

use clap::Parser;
use streambeam_core::FrameReassembler;
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};

use client::connect;
use play::{AudioPlayer, VideoPlayer};

#[derive(Parser, Debug)]
#[command(name = "playclient")]
struct Opt {
    /// Pairing payload from fakehost (`sbm1:...`).
    payload: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let opt = Opt::parse();
    if let Err(e) = run(&opt.payload).await {
        eprintln!("playclient error: {e}");
        std::process::exit(1);
    }
}

async fn run(payload: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session = connect(payload).await?;
    eprintln!(
        "paired — playing {}x{} @ {} fps ({}) + 48 kHz stereo audio",
        session.width, session.height, session.fps, session.codec
    );
    eprintln!("close the video window or press Ctrl+C to stop");

    let title = format!("StreamBeam {}x{}", session.width, session.height);
    let mut video = VideoPlayer::spawn(&title)?;
    let mut audio = AudioPlayer::spawn()?;
    let mut asm = FrameReassembler::default();
    let mut frames = 0u64;

    loop {
        if video.finished() {
            eprintln!("video window closed");
            break;
        }

        let datagram = match tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram())
            .await
        {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => break,
            Err(_) => continue,
        };

        let header = MediaHeader::decode(&datagram)?;
        if header.channel == CHANNEL_AUDIO {
            if datagram.len() > 16 {
                if let Err(e) = audio.push_opus(&datagram[16..]) {
                    if e.kind() != std::io::ErrorKind::BrokenPipe {
                        return Err(e.into());
                    }
                }
            }
            continue;
        }

        if let Some((_frame_id, frame)) = asm.push_datagram(&datagram)? {
            if let Err(e) = video.push_access_unit(&frame) {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    eprintln!("video window closed");
                    break;
                }
                return Err(e.into());
            }
            frames += 1;
            if frames == 1 || frames % 30 == 0 {
                eprintln!("playing frame {frames} ({} bytes)", frame.len());
            }
        }
    }

    eprintln!("done — {frames} video frames");
    Ok(())
}
