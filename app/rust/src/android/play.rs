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
    access_unit_is_keyframe, access_unit_to_avcc_sample, access_unit_without_sei,
    avcc_from_sps_pps, connect_as_identity, connect_known, extract_avc_decoder_config,
    idr_sample_avcc, nal_with_start_code, request_idr, save_saved_host, ClientSession,
    FrameReassembler, SavedHost,
};
use streambeam_proto::media::{MediaHeader, CHANNEL_AUDIO};

use crate::api::StreamStats;
use crate::store::client_identity_path;

const TIMEOUT: Duration = Duration::from_micros(30_000);
const BUFFER_FLAG_KEY_FRAME: u32 = 1;

#[derive(Copy, Clone, Debug)]
enum FeedMode {
    /// `csd-0`/`csd-1` hold start-code SPS/PPS; input buffers are Annex-B.
    AnnexB,
    /// `csd-0` holds avcC; input buffers are length-prefixed AVCC.
    Avcc,
}

fn codec_err(step: &str, err: impl std::fmt::Display) -> String {
    log::error!("streambeam {step}: {err}");
    format!("Video decoder {step} failed: {err}")
}

fn media_err_label(err: ndk::media_error::MediaError) -> &'static str {
    use ndk::media_error::MediaError;
    match err {
        MediaError::ErrorUnknown => "unknown codec error (bad video config or surface)",
        MediaError::ErrorMalformed => "malformed video stream",
        MediaError::ErrorUnsupported => "unsupported video format",
        MediaError::ErrorInvalidParameter => "invalid decoder parameter",
        MediaError::ErrorInvalidOperation => "invalid decoder operation",
        _ => "codec error",
    }
}

fn try_configure_decoder(
    window: &NativeWindow,
    width: i32,
    height: i32,
    set_csd: impl FnOnce(&mut MediaFormat),
) -> Result<MediaCodec, ndk::media_error::MediaError> {
    let mut codec = MediaCodec::from_decoder_type("video/avc")
        .ok_or(ndk::media_error::MediaError::ErrorUnsupported)?;
    let mut format = MediaFormat::new();
    format.set_str("mime", "video/avc");
    format.set_i32("width", width);
    format.set_i32("height", height);
    format.set_i32("max-input-size", 512 * 1024);
    set_csd(&mut format);
    codec.configure(&format, Some(window), MediaCodecDirection::Decoder)?;
    Ok(codec)
}

fn start_decoder(
    window: &NativeWindow,
    width: i32,
    height: i32,
    sps: &[u8],
    pps: &[u8],
    avcc: &[u8],
) -> Result<(MediaCodec, FeedMode), String> {
    let mut attempts: Vec<String> = Vec::new();
    let csd0_sc = nal_with_start_code(sps);
    let csd1_sc = nal_with_start_code(pps);

    match try_configure_decoder(window, width, height, |fmt| {
        fmt.set_buffer("csd-0", &csd0_sc);
        fmt.set_buffer("csd-1", &csd1_sc);
    }) {
        Ok(codec) => {
            log::info!("streambeam decoder configured with Annex-B csd-0/csd-1");
            return Ok((codec, FeedMode::AnnexB));
        }
        Err(e) => attempts.push(format!("Annex-B CSD: {}", media_err_label(e))),
    }

    match try_configure_decoder(window, width, height, |fmt| {
        fmt.set_buffer("csd-0", sps);
        fmt.set_buffer("csd-1", pps);
    }) {
        Ok(codec) => {
            log::info!("streambeam decoder configured with raw SPS/PPS csd");
            return Ok((codec, FeedMode::AnnexB));
        }
        Err(e) => attempts.push(format!("raw SPS/PPS: {}", media_err_label(e))),
    }

    match try_configure_decoder(window, width, height, |fmt| {
        fmt.set_buffer("csd-0", avcc);
    }) {
        Ok(codec) => {
            log::info!("streambeam decoder configured with avcC csd-0");
            return Ok((codec, FeedMode::Avcc));
        }
        Err(e) => attempts.push(format!("avcC: {}", media_err_label(e))),
    }

    log::error!("streambeam all decoder configure attempts failed: {attempts:?}");
    Err(format!(
        "Video decoder could not start on this phone ({})",
        attempts.join("; ")
    ))
}

fn prime_sample(mode: FeedMode, config: &[u8]) -> Result<Vec<u8>, String> {
    match mode {
        FeedMode::AnnexB => {
            let au = access_unit_without_sei(config);
            if au.is_empty() {
                return Err("Host keyframe was empty.".into());
            }
            Ok(au)
        }
        FeedMode::Avcc => idr_sample_avcc(config)
            .ok_or_else(|| "Host video config is missing an IDR keyframe.".into()),
    }
}

fn encode_frame(mode: FeedMode, frame: &[u8]) -> Vec<u8> {
    match mode {
        FeedMode::AnnexB => frame.to_vec(),
        FeedMode::Avcc => access_unit_to_avcc_sample(frame),
    }
}

pub async fn run(payload: String, native_window: i64, max_video_frames: u32) -> Result<StreamStats, String> {
    log::info!("streambeam connecting with pairing payload");
    let session = connect_as_identity(
        &payload,
        "streambeam-android",
        Some(&client_identity_path()),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam connect failed: {e}");
        format!("Could not reach host: {e}")
    })?;

    if let Ok(host) = SavedHost::from_pair_payload(&payload) {
        let _ = save_saved_host(&crate::store::saved_host_path(), &host);
    }

    log::info!("streambeam connected, starting playback");
    play_session(session, native_window, max_video_frames).await
}

pub async fn run_known(
    host: SavedHost,
    addr: SocketAddr,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    log::info!("streambeam reconnecting to saved host at {addr}");
    let session = connect_known(
        host.host_pubkey,
        addr,
        "streambeam-android",
        &client_identity_path(),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam reconnect failed: {e}");
        format!("Could not reach host: {e}")
    })?;
    play_session(session, native_window, max_video_frames).await
}

async fn play_session(
    mut session: ClientSession,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let runtime = tokio::runtime::Handle::current();
    let input = session
        .input
        .take()
        .ok_or("input stream not open")?;
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::input::set_sender(Some(input_tx));

    let input_task = runtime.spawn(async move {
        let mut input_send = input;
        while let Some(ev) = input_rx.recv().await {
            if streambeam_net::write_msg(&mut input_send, &ev).await.is_err() {
                break;
            }
        }
    });

    let result = tokio::task::spawn_blocking(move || {
        play_session_blocking(session, native_window, max_video_frames, runtime)
    })
    .await
    .map_err(|e| format!("playback task failed: {e}"))?;

    crate::input::set_sender(None);
    input_task.abort();
    result
}

fn play_session_blocking(
    mut session: ClientSession,
    native_window: i64,
    max_video_frames: u32,
    runtime: tokio::runtime::Handle,
) -> Result<StreamStats, String> {
    if let Err(e) = runtime.block_on(request_idr(&mut session)) {
        log::warn!("streambeam request_idr failed (continuing): {e}");
    }

    let config = session
        .codec_config
        .as_deref()
        .ok_or("Host did not send video configuration (CodecConfig).")?;
    let (sps, pps) = extract_avc_decoder_config(config)
        .ok_or("Host video config is missing H.264 SPS/PPS.")?;
    let avcc = avcc_from_sps_pps(&sps, &pps).ok_or("Could not build H.264 decoder config.")?;

    let window = unsafe {
        let ptr = NonNull::new(native_window as *mut ndk_sys::ANativeWindow)
            .ok_or("Video surface is not ready.")?;
        NativeWindow::from_ptr(ptr)
    };

    let width = i32::from(session.width);
    let height = i32::from(session.height);
    let (mut codec, feed_mode) = start_decoder(&window, width, height, &sps, &pps, &avcc)?;
    codec.start().map_err(|e| codec_err("start", media_err_label(e)))?;

    if let Err(e) = window.set_buffers_geometry(width, height, None) {
        log::warn!("streambeam set_buffers_geometry after configure: {e}");
    }

    let prime_sample = prime_sample(feed_mode, config)?;
    log::info!(
        "streambeam decoder started {}x{} — priming with {} byte keyframe ({:?})",
        session.width,
        session.height,
        prime_sample.len(),
        feed_mode
    );

    feed_sample(&mut codec, &prime_sample, 0, BUFFER_FLAG_KEY_FRAME)?;
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
    let mut pts_us: u64 = 33_000;

    while stats.video_frames < target {
        drain_output(&mut codec)?;

        let datagram = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram()).await
            })
            .map_err(|_| "Timed out waiting for video from host.".to_string())?
            .map_err(|e| format!("Stream read failed: {e}"))?;

        let header = MediaHeader::decode(&datagram).map_err(|e| e.to_string())?;
        if header.channel == CHANNEL_AUDIO {
            stats.audio_packets += 1;
            continue;
        }

        if let Some((_id, frame)) = asm.push_datagram(&datagram).map_err(|e| e.to_string())? {
            let frame = access_unit_without_sei(&frame);
            if frame.is_empty() {
                continue;
            }
            let sample = encode_frame(feed_mode, &frame);
            if sample.is_empty() {
                continue;
            }
            let flags = if access_unit_is_keyframe(&frame) {
                BUFFER_FLAG_KEY_FRAME
            } else {
                0
            };
            feed_sample(&mut codec, &sample, pts_us, flags)?;
            pts_us += 33_000;
            stats.video_frames = asm.stats().frames_complete;
            drain_output(&mut codec)?;
        }
    }

    codec.stop().ok();
    Ok(stats)
}

fn feed_sample(
    codec: &mut MediaCodec,
    data: &[u8],
    pts_us: u64,
    flags: u32,
) -> Result<(), String> {
    let input = codec
        .dequeue_input_buffer(TIMEOUT)
        .map_err(|e| codec_err("dequeue input", e))?;
    let DequeuedInputBufferResult::Buffer(mut input_buf) = input else {
        return Err("Video decoder input buffer unavailable.".into());
    };
    let buf = input_buf.buffer_mut();
    if buf.len() < data.len() {
        return Err(format!(
            "Video decoder input buffer too small ({} < {})",
            buf.len(),
            data.len()
        ));
    }
    for (dst, &byte) in buf[..data.len()].iter_mut().zip(data.iter()) {
        dst.write(byte);
    }
    codec
        .queue_input_buffer(input_buf, 0, data.len(), pts_us, flags)
        .map_err(|e| codec_err("queue input", e))?;
    Ok(())
}

fn drain_output(codec: &mut MediaCodec) -> Result<(), String> {
    loop {
        match codec
            .dequeue_output_buffer(TIMEOUT)
            .map_err(|e| codec_err("dequeue output", e))?
        {
            DequeuedOutputBufferInfoResult::Buffer(out) => {
                let flags = out.info().flags();
                let render = flags & 0x2 == 0;
                codec
                    .release_output_buffer(out, render)
                    .map_err(|e| codec_err("release output", e))?;
                if flags & 0x4 != 0 {
                    break;
                }
            }
            DequeuedOutputBufferInfoResult::TryAgainLater => break,
            DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                let _ = codec.output_format();
                log::info!("streambeam decoder output format changed");
            }
            DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
        }
    }
    Ok(())
}
