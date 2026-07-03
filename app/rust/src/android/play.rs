//! Decode Annex-B H.264 access units to an Android Surface via MediaCodec.

use std::net::SocketAddr;
use std::ptr::NonNull;
use std::time::Duration;

use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
    MediaFormat,
};
use ndk::native_window::NativeWindow;
use streambeam_core::{
    annex_b::{access_unit_is_keyframe, extract_avc_decoder_config, nal_with_start_code},
    connect_as_identity, connect_known, request_idr, save_saved_host, ClientSession,
    FrameReassembler, SavedHost,
};
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};

use crate::api::StreamStats;
use crate::store::client_identity_path;

const TIMEOUT: Duration = Duration::from_micros(30_000);
const BUFFER_FLAG_KEY_FRAME: u32 = 1;

pub async fn run(payload: String, native_window: i64, max_video_frames: u32) -> Result<StreamStats, String> {
    let session = connect_as_identity(
        &payload,
        "streambeam-android",
        Some(&client_identity_path()),
    )
    .await
    .map_err(|e| e.to_string())?;

    if let Ok(host) = SavedHost::from_pair_payload(&payload) {
        let _ = save_saved_host(&crate::store::saved_host_path(), &host);
    }

    play_session(session, native_window, max_video_frames).await
}

pub async fn run_known(
    host: SavedHost,
    addr: SocketAddr,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let session = connect_known(
        host.host_pubkey,
        addr,
        "streambeam-android",
        &client_identity_path(),
    )
    .await
    .map_err(|e| e.to_string())?;
    play_session(session, native_window, max_video_frames).await
}

async fn play_session(
    session: ClientSession,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(play_session_inner(session, native_window, max_video_frames))
    })
    .await
    .map_err(|e| format!("playback task failed: {e}"))?
}

async fn play_session_inner(
    mut session: ClientSession,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let _ = request_idr(&mut session).await;

    let config = session
        .codec_config
        .as_deref()
        .ok_or("host did not send CodecConfig")?;
    let (sps, pps) = extract_avc_decoder_config(config).ok_or("missing SPS/PPS in CodecConfig")?;
    let csd0 = nal_with_start_code(&sps);
    let csd1 = nal_with_start_code(&pps);

    let window = unsafe {
        let ptr = NonNull::new(native_window as *mut ndk_sys::ANativeWindow)
            .ok_or("invalid native window")?;
        NativeWindow::from_ptr(ptr)
    };
    window
        .set_buffers_geometry(
            i32::from(session.width),
            i32::from(session.height),
            None,
        )
        .map_err(|e| format!("surface geometry: {e}"))?;

    let mut codec = MediaCodec::from_decoder_type("video/avc")
        .ok_or("no AVC decoder available")?;
    let mut format = MediaFormat::new();
    format.set_str("mime", "video/avc");
    format.set_i32("width", i32::from(session.width));
    format.set_i32("height", i32::from(session.height));
    format.set_i32("max-input-size", 512 * 1024);
    format.set_buffer("csd-0", &csd0);
    format.set_buffer("csd-1", &csd1);

    codec
        .configure(&format, Some(&window), MediaCodecDirection::Decoder)
        .map_err(|e| e.to_string())?;
    codec.start().map_err(|e| e.to_string())?;

    // Match ffplay `prime()`: feed the host CodecConfig access unit (SPS/PPS/IDR) before
    // network frames so the decoder has a keyframe even if the first live packet is a P-frame.
    feed_access_unit(&mut codec, config, 0, BUFFER_FLAG_KEY_FRAME)?;
    drain_output(&mut codec)?;

    let mut stats = StreamStats {
        width: session.width,
        height: session.height,
        fps: session.fps,
        codec: session.codec.clone(),
        video_frames: 0,
        audio_packets: 0,
    };

    let mut asm = FrameReassembler::default();
    let target = if max_video_frames == 0 {
        u64::MAX
    } else {
        u64::from(max_video_frames.max(1))
    };
    let mut pts_us: u64 = 0;

    while stats.video_frames < target {
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
            let flags = if access_unit_is_keyframe(&frame) {
                BUFFER_FLAG_KEY_FRAME
            } else {
                0
            };
            feed_access_unit(&mut codec, &frame, pts_us, flags)?;
            pts_us += 33_000;
            stats.video_frames = asm.stats().frames_complete;
            drain_output(&mut codec)?;
        }
    }

    codec.stop().ok();
    Ok(stats)
}

fn feed_access_unit(
    codec: &mut MediaCodec,
    data: &[u8],
    pts_us: u64,
    flags: u32,
) -> Result<(), String> {
    let input = codec
        .dequeue_input_buffer(TIMEOUT)
        .map_err(|e| e.to_string())?;
    let DequeuedInputBufferResult::Buffer(mut input_buf) = input else {
        return Err("MediaCodec input buffer unavailable".into());
    };
    let buf = input_buf.buffer_mut();
    if buf.len() < data.len() {
        return Err(format!(
            "input buffer too small ({} < {})",
            buf.len(),
            data.len()
        ));
    }
    for (dst, &byte) in buf[..data.len()].iter_mut().zip(data.iter()) {
        dst.write(byte);
    }
    codec
        .queue_input_buffer(input_buf, 0, data.len(), pts_us, flags)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn drain_output(codec: &mut MediaCodec) -> Result<(), String> {
    loop {
        match codec
            .dequeue_output_buffer(TIMEOUT)
            .map_err(|e| e.to_string())?
        {
            DequeuedOutputBufferInfoResult::Buffer(out) => {
                let flags = out.info().flags();
                let render = flags & 0x2 == 0;
                codec
                    .release_output_buffer(out, render)
                    .map_err(|e| e.to_string())?;
                if flags & 0x4 != 0 {
                    break;
                }
            }
            DequeuedOutputBufferInfoResult::TryAgainLater => break,
            DequeuedOutputBufferInfoResult::OutputFormatChanged
            | DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
        }
    }
    Ok(())
}
