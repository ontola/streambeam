//! ffplay-backed video and audio output (uses system ffplay, no native codec link).

use std::io::{Write, ErrorKind};
use std::process::{Child, ChildStdin, Command, Stdio};

use opus::{Channels, Decoder};

const SAMPLE_RATE: u32 = 48_000;

pub struct VideoPlayer {
    _child: Child,
    stdin: ChildStdin,
}

impl VideoPlayer {
    pub fn spawn(title: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut child = Command::new("ffplay")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-framedrop",
                "-window_title",
                title,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to launch ffplay for video ({e}); install ffmpeg"))?;
        let stdin = child.stdin.take().expect("ffplay stdin");
        Ok(Self {
            _child: child,
            stdin,
        })
    }

    pub fn push_access_unit(&mut self, annex_b: &[u8]) -> std::io::Result<()> {
        self.stdin.write_all(annex_b)?;
        self.stdin.flush()
    }

    pub fn finished(&mut self) -> bool {
        matches!(
            self._child.try_wait(),
            Ok(Some(_)) | Err(_)
        )
    }
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
                "pipe:0",
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
        matches!(
            self._child.try_wait(),
            Ok(Some(_)) | Err(_)
        )
    }
}
