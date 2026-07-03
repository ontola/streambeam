//! Decode Annex-B H.264 access units to an Android Surface via MediaCodec.

use std::time::Duration;

use ndk::media::media_codec::{MediaCodec, MediaCodecBufferInfo, MediaFormat};
use ndk::native_window::NativeWindow;
use streambeam_core::{
    annex_b::{extract_avc_decoder_config, nal_with_start_code},
    connect_as, FrameReassembler,
};
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};

use crate::api::StreamStats;

const TIMEOUT_US: i64 = 10_000;

pub async fn run(payload: String, native_window: i64, max_video_frames: u32) -> Result<StreamStats, String> {
    let session = connect_as(&payload, "streambeam-android")
        .await
        .map_err(|e| e.to_string())?;

    let config = session
        .codec_config
        .as_deref()
        .ok_or("host did not send CodecConfig")?;
    let (sps, pps) = extract_avc_decoder_config(config).ok_or("missing SPS/PPS in CodecConfig")?;
    let csd0 = nal_with_start_code(&sps);
    let csd1 = nal_with_start_code(&pps);

    let window = unsafe {
        NativeWindow::from_ptr(native_window as *mut _)
            .ok_or("invalid native window")?
    };

    let mut codec = MediaCodec::from_decoder_type("video/avc").map_err(|e| e.to_string())?;
    let mut format = MediaFormat::new();
    format
        .set_str("mime", "video/avc")
        .map_err(|e| e.to_string())?;
    format
        .set_i32("width", i32::from(session.width))
        .map_err(|e| e.to_string())?;
    format
        .set_i32("height", i32::from(session.height))
        .map_err(|e| e.to_string())?;
    format
        .set_buffer("csd-0", &csd0, 0, csd0.len())
        .map_err(|e| e.to_string())?;
    format
        .set_buffer("csd-1", &csd1, 0, csd1.len())
        .map_err(|e| e.to_string())?;

    codec
        .configure(&format, Some(&window), Default::default(), 0)
        .map_err(|e| e.to_string())?;
    codec.start().map_err(|e| e.to_string())?;

    let mut stats = StreamStats {
        width: session.width,
        height: session.height,
        fps: session.fps,
        codec: session.codec.clone(),
        video_frames: 0,
        audio_packets: 0,
    };

    let mut asm = FrameReassembler::default();
    let target = max_video_frames.max(1);
    let mut pts_us: i64 = 0;

    while stats.video_frames < u64::from(target) {
        drain_output(&mut codec)?;

        let datagram = tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram())
            .await
            .map_err(|_| "timed out waiting for media".to_string())?
            .map_err(|e| e.to_string())?;

        let header = MediaHeader::decode(&datagram).map_err(|e| e.to_string())?;
        if header.channel == CHANNEL_AUDIO {
            stats.audio_packets += 1;
            continue;
        }

        if let Some((_id, frame)) = asm.push_datagram(&datagram).map_err(|e| e.to_string())? {
            feed_access_unit(&mut codec, &frame, pts_us)?;
            pts_us += 33_000;
            stats.video_frames = asm.stats().frames_complete;
            drain_output(&mut codec)?;
        }
    }

    codec.stop().ok();
    Ok(stats)
}

fn feed_access_unit(codec: &mut MediaCodec, data: &[u8], pts_us: i64) -> Result<(), String> {
    let (index, mut buf) = codec
        .dequeue_input_buffer(TIMEOUT_US)
        .map_err(|e| e.to_string())?
        .ok_or("MediaCodec input buffer unavailable")?;
    if buf.len() < data.len() {
        return Err(format!(
            "input buffer too small ({} < {})",
            buf.len(),
            data.len()
        ));
    }
    buf[..data.len()].copy_from_slice(data);
    codec
        .queue_input_buffer(index, 0, data.len(), pts_us, 0)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn drain_output(codec: &mut MediaCodec) -> Result<(), String> {
    loop {
        match codec
            .dequeue_output_buffer(TIMEOUT_US)
            .map_err(|e| e.to_string())?
        {
            Some((index, info)) => {
                let render = !info.is_codec_config();
                codec
                    .release_output_buffer(index, render)
                    .map_err(|e| e.to_string())?;
                if info.is_end_of_stream() {
                    break;
                }
            }
            None => break,
        }
    }
    Ok(())
}

trait BufferInfoExt {
    fn is_codec_config(&self) -> bool;
    fn is_end_of_stream(&self) -> bool;
}

impl BufferInfoExt for MediaCodecBufferInfo {
    fn is_codec_config(&self) -> bool {
        self.flags & 0x2 != 0
    }

    fn is_end_of_stream(&self) -> bool {
        self.flags & 0x4 != 0
    }
}
