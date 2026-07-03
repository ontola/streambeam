//! ffplay-backed video and audio output (uses system ffplay, no native codec link).

use std::io::{ErrorKind, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::Duration;

use opus::{Channels, Decoder};

const SAMPLE_RATE: u32 = 48_000;

enum VideoBackend {
    Ffplay { child: Child, stdin: ChildStdin },
    Mpv { child: Child, stdin: ChildStdin },
}

pub struct VideoPlayer {
    backend: VideoBackend,
}

impl VideoPlayer {
    pub fn spawn(
        title: &str,
        width: u16,
        height: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let prefer_mpv = std::env::var("STREAMBEAM_PLAYER")
            .map(|v| v.eq_ignore_ascii_case("mpv"))
            .unwrap_or(false);

        let mut player = if prefer_mpv {
            spawn_mpv(title, width, height).or_else(|e| {
                eprintln!("mpv failed ({e}), falling back to ffplay");
                spawn_ffplay(title, width, height)
            })
        } else {
            spawn_ffplay(title, width, height).or_else(|e| {
                eprintln!("ffplay failed ({e}), trying mpv");
                spawn_mpv(title, width, height)
            })
        }?;

        thread::sleep(Duration::from_millis(250));
        if player.finished() {
            return Err(
                "video player exited immediately — run with STREAMBEAM_FFPLAY_VERBOSE=1 for details"
                    .into(),
            );
        }

        Ok(player)
    }

    pub fn prime(&mut self, annex_b: &[u8]) -> std::io::Result<()> {
        self.push_access_unit(annex_b)
    }

    pub fn push_access_unit(&mut self, annex_b: &[u8]) -> std::io::Result<()> {
        match &mut self.backend {
            VideoBackend::Ffplay { stdin, .. } | VideoBackend::Mpv { stdin, .. } => {
                stdin.write_all(annex_b)?;
                stdin.flush()
            }
        }
    }

    pub fn finished(&mut self) -> bool {
        let child = match &mut self.backend {
            VideoBackend::Ffplay { child, .. } | VideoBackend::Mpv { child, .. } => child,
        };
        matches!(child.try_wait(), Ok(Some(_)))
    }
}

fn spawn_ffplay(
    title: &str,
    width: u16,
    height: u16,
) -> Result<VideoPlayer, Box<dyn std::error::Error + Send + Sync>> {
    let verbose = std::env::var("STREAMBEAM_FFPLAY_VERBOSE").is_ok();
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        if verbose {
            "info".into()
        } else {
            "warning".into()
        },
        "-f".into(),
        "h264".into(),
        "-i".into(),
        "-".into(),
        "-fflags".into(),
        "nobuffer+flush_packets".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "524288".into(),
        "-analyzeduration".into(),
        "500000".into(),
        "-framedrop".into(),
        "-alwaysontop".into(),
        "-window_title".into(),
        title.to_string(),
    ];

    let stderr = if verbose {
        Stdio::inherit()
    } else {
        Stdio::null()
    };

    let mut child = Command::new("ffplay")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("failed to launch ffplay ({e}); install ffmpeg"))?;

    let pid = child.id();
    eprintln!(
        "ffplay video pid={pid} — look for \"{title}\" ({width}x{height})"
    );

    let stdin = child.stdin.take().expect("ffplay stdin");
    Ok(VideoPlayer {
        backend: VideoBackend::Ffplay { child, stdin },
    })
}

fn spawn_mpv(
    title: &str,
    width: u16,
    height: u16,
) -> Result<VideoPlayer, Box<dyn std::error::Error + Send + Sync>> {
    let mut child = Command::new("mpv")
        .args([
            "--no-terminal",
            "--no-audio",
            "--untimed",
            "--no-correct-pts",
            "--demuxer=lavf",
            "--demuxer-lavf-format=h264",
            "--demuxer-lavf-o=probesize=524288",
            "--title",
            title,
            "--geometry",
            &format!("{width}x{height}"),
            "--ontop",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch mpv ({e}); brew install mpv"))?;

    let pid = child.id();
    eprintln!("mpv video pid={pid} — look for \"{title}\" ({width}x{height})");

    let stdin = child.stdin.take().expect("mpv stdin");
    Ok(VideoPlayer {
        backend: VideoBackend::Mpv { child, stdin },
    })
}

pub struct AudioPlayer {
    _child: Child,
    stdin: ChildStdin,
    decoder: Decoder,
    pcm: Vec<i16>,
}

impl AudioPlayer {
    pub fn spawn() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut child = Command::new("ffplay")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nodisp",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to launch ffplay for audio ({e}); install ffmpeg"))?;
        let stdin = child.stdin.take().expect("ffplay stdin");
        Ok(Self {
            _child: child,
            stdin,
            decoder: Decoder::new(SAMPLE_RATE, Channels::Stereo)?,
            pcm: vec![0i16; 5760 * 2],
        })
    }

    pub fn push_opus(&mut self, packet: &[u8]) -> std::io::Result<()> {
        let samples = match self.decoder.decode(packet, &mut self.pcm, false) {
            Ok(n) => n,
            Err(e) => {
                if e.code() == opus::ErrorCode::InvalidPacket {
                    return Ok(());
                }
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("opus decode: {e}"),
                ));
            }
        };
        for sample in &self.pcm[..samples * 2] {
            self.stdin.write_all(&sample.to_le_bytes())?;
        }
        self.stdin.flush()
    }

    pub fn finished(&mut self) -> bool {
        matches!(self._child.try_wait(), Ok(Some(_)))
    }
}
