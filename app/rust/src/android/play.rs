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
use streambeam_proto::input::InputEvent;
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

fn coalesce_input_batch(first: InputEvent, rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputEvent>) -> Vec<InputEvent> {
    let mut batch = vec![first];
    while let Ok(next) = rx.try_recv() {
        batch.push(next);
        if batch.len() >= 64 {
            break;
        }
    }

    let mut out = Vec::with_capacity(batch.len());
    let mut i = 0;
    while i < batch.len() {
        match batch[i] {
            InputEvent::MouseMove { dx, dy } => {
                let mut sum_dx = i32::from(dx);
                let mut sum_dy = i32::from(dy);
                let mut j = i + 1;
                while j < batch.len() {
                    match batch[j] {
                        InputEvent::MouseMove { dx, dy } => {
                            sum_dx = sum_dx.saturating_add(i32::from(dx));
                            sum_dy = sum_dy.saturating_add(i32::from(dy));
                            j += 1;
                        }
                        _ => break,
                    }
                }
                out.push(InputEvent::MouseMove {
                    dx: sum_dx.clamp(-32768, 32767) as i16,
                    dy: sum_dy.clamp(-32768, 32767) as i16,
                });
                i = j;
            }
            ref other => {
                out.push(other.clone());
                i += 1;
            }
        }
    }
    out
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

pub async fn run_discovered(
    host_name: String,
    host_pubkey: Vec<u8>,
    addr: String,
    native_window: i64,
    max_video_frames: u32,
) -> Result<StreamStats, String> {
    let pubkey: [u8; 32] = host_pubkey
        .try_into()
        .map_err(|_| "host public key must be 32 bytes".to_string())?;
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("invalid host address: {e}"))?;

    log::info!("streambeam connecting to LAN host {host_name} at {addr}");
    let session = connect_known(
        pubkey,
        addr,
        "streambeam-android",
        &client_identity_path(),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam LAN connect failed: {e}");
        format!("Could not reach host: {e}")
    })?;

    let host = SavedHost {
        host_name,
        host_pubkey: pubkey,
        addrs: vec![addr],
    };
    let _ = save_saved_host(&crate::store::saved_host_path(), &host);

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
    crate::playback::reset();
    let runtime = tokio::runtime::Handle::current();
    let input = session
        .input
        .take()
        .ok_or("input stream not open")?;
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::input::set_sender(Some(input_tx));

    let input_task = runtime.spawn(async move {
        let mut input_send = input;
        while let Some(first) = input_rx.recv().await {
            for ev in coalesce_input_batch(first, &mut input_rx) {
                if streambeam_net::write_msg(&mut input_send, &ev).await.is_err() {
                    return;
                }
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
    crate::playback::set_session(
        session.width,
        session.height,
        session.fps,
        session.codec.clone(),
    );
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
    // How many decodable (non-keyframe) completions we've skipped in a row
    // because we were catching up on a backlog — see the loop body.
    let mut consecutive_skipped = 0u32;
    const IDR_RESYNC_THRESHOLD: u32 = 20;
    // Don't hammer the host with repeated keyframe requests: a forced
    // keyframe is itself a large burst (150-300 KB even at 1080p) that can
    // cause the very backlog it's meant to fix if requested too often —
    // without this, a host that's merely a bit slow (not catastrophically
    // behind) oscillates between huge keyframe bursts and near-stalls instead
    // of settling down.
    const IDR_REQUEST_COOLDOWN: Duration = Duration::from_secs(3);
    let mut last_idr_request: Option<std::time::Instant> = None;

    while stats.video_frames < target {
        drain_output(&mut codec)?;

        let first = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram()).await
            })
            .map_err(|_| "Timed out waiting for video from host.".to_string())?
            .map_err(|e| format!("Stream read failed: {e}"))?;

        // Greedily grab anything ALREADY sitting in the receive queue too
        // (a near-zero wait; genuinely new arrivals won't show up in time).
        // More than one datagram back-to-back means decode/render fell
        // behind real time since the last iteration — plain FIFO processing
        // of an ever-growing backlog is exactly how you end up watching
        // content from a minute ago with no way to ever catch up.
        let mut batch = vec![first];
        while let Ok(Ok(next)) = runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(2), session.conn.read_datagram()).await
        }) {
            batch.push(next);
            if batch.len() >= 64 {
                break; // safety valve against an unbounded drain loop
            }
        }
        let behind = batch.len() > 1;
        let last_index = batch.len() - 1;

        for (i, datagram) in batch.iter().enumerate() {
            crate::playback::note_bytes_received(datagram.len() as u64);

            let header = MediaHeader::decode(datagram).map_err(|e| e.to_string())?;
            if header.channel == CHANNEL_AUDIO {
                stats.audio_packets += 1;
                continue;
            }

            match asm.push_datagram(datagram) {
                Ok(Some((_id, frame))) => {
                    let frame = access_unit_without_sei(&frame);
                    if frame.is_empty() {
                        continue;
                    }
                    let is_keyframe = access_unit_is_keyframe(&frame);

                    // Always decode a keyframe (it resets the reference chain and is
                    // worth showing even a little late). Otherwise, while behind,
                    // only decode the freshest completion in this batch — the rest
                    // are already stale by the time we'd finish them, so decoding
                    // them would just burn time we need to spend catching up.
                    if behind && !is_keyframe && i != last_index {
                        consecutive_skipped += 1;
                        stats.video_frames = asm.stats().frames_complete;
                        continue;
                    }

                    let sample = encode_frame(feed_mode, &frame);
                    if sample.is_empty() {
                        continue;
                    }
                    let flags = if is_keyframe {
                        BUFFER_FLAG_KEY_FRAME
                    } else {
                        0
                    };
                    feed_sample(&mut codec, &sample, pts_us, flags)?;
                    pts_us += 33_000;
                    stats.video_frames = asm.stats().frames_complete;
                    drain_output(&mut codec)?;
                    consecutive_skipped = 0;
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("streambeam frame reassembly failed: {e}");
                }
            }
        }

        // We've been skipping non-keyframes for a while (they depend on a
        // reference chain we're no longer decoding) — ask the host for a
        // fresh keyframe so the picture recovers instead of degrading. Only
        // if we're not still cooling down from the last request, so a host
        // that's persistently a little behind doesn't get spammed.
        if consecutive_skipped >= IDR_RESYNC_THRESHOLD {
            consecutive_skipped = 0;
            let cooled_down = last_idr_request
                .map(|t| t.elapsed() >= IDR_REQUEST_COOLDOWN)
                .unwrap_or(true);
            if cooled_down {
                last_idr_request = Some(std::time::Instant::now());
                if let Err(e) = runtime.block_on(request_idr(&mut session)) {
                    log::warn!("streambeam catch-up request_idr failed (continuing): {e}");
                }
            }
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
                if render {
                    crate::playback::note_frame_rendered();
                }
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
