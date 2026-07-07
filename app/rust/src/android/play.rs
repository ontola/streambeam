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
    ClientSession, FrameReassembler, LossReport, SavedHost, access_unit_is_keyframe,
    access_unit_to_avcc_sample, access_unit_without_sei, avcc_from_sps_pps, connect_as_identity,
    connect_known, extract_avc_decoder_config, idr_sample_avcc, nal_with_start_code, request_idr,
    request_invalidate_ref_frames, save_saved_host,
};
use streambeam_proto::control::VideoCaps;
use streambeam_proto::media::{CHANNEL_AUDIO, MediaHeader};

use crate::api::StreamStats;
use crate::store::client_identity_path;

// Used for MediaCodec dequeue_input_buffer/dequeue_output_buffer calls made
// synchronously on the same thread that also drives the network read loop
// (`play_session_blocking`). A "no buffer ready yet" dequeue burns the full
// timeout doing nothing — at 30ms that alone can exceed a real-time frame
// budget (33ms at 30fps) per call, and there are multiple such calls per
// loop iteration. Once the client falls even slightly behind, this caps how
// fast it can ever catch back up, turning a transient stall into a
// permanent backlog. Keep this near-instant; a real "nothing is ready" case
// just means we go around the loop again immediately.
const TIMEOUT: Duration = Duration::from_micros(2_000);
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
    // Without this, many vendor decoders default to a throughput-oriented
    // pipeline that buffers several frames internally before emitting any
    // output (better average efficiency, worse latency). If the decoder's
    // real drain rate falls behind our real-time 30fps feed, the input
    // buffer pool exhausts and `feed_sample`'s dequeue_input_buffer times
    // out — which hard-errors the whole session, not just one frame. This
    // is the same MediaFormat.KEY_LOW_LATENCY Moonlight's Android client
    // sets for exactly this reason. Ignored (not fatal) on API < 30.
    format.set_i32("low-latency", 1);
    // Desktop-capture content is genuinely full-range RGB (true black is 0,
    // not 16) — but NVENC's H.264 output doesn't signal VUI color-range info
    // (that lives inside the codec-specific config struct we deliberately
    // treat as opaque, see nvenc.rs), so a decoder falling back to the
    // standard broadcast-video assumption (limited/16-235) stretches the
    // full-range signal outward, crushing near-black values to true black
    // and clipping near-white ones — most visible on a full keyframe
    // repaint, e.g. a UI hover highlight. Force full range explicitly to
    // match the actual source instead of guessing. `COLOR_RANGE_FULL = 1`
    // per Android's MediaFormat docs; API 28+, harmlessly ignored below that.
    format.set_i32("color-range", 1);
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

/// `max_width`/`max_height`/`max_fps` come from Dart's `dart:ui` `Display`
/// query (this device's real screen, not a guess) — see `stream_player.dart`.
/// `0` in any field falls back to a safe default rather than an invalid
/// `SessionRequest` (e.g. a `View`/`Display` that hasn't reported real values
/// yet at call time).
fn video_caps(max_width: u16, max_height: u16, max_fps: u8) -> VideoCaps {
    let fallback = streambeam_core::fallback_video_caps();
    VideoCaps {
        h264: true,
        hevc: false,
        av1: false,
        max_width: if max_width > 0 {
            max_width
        } else {
            fallback.max_width
        },
        max_height: if max_height > 0 {
            max_height
        } else {
            fallback.max_height
        },
        max_fps: if max_fps > 0 { max_fps } else { fallback.max_fps },
    }
}

pub async fn run(
    payload: String,
    native_window: i64,
    max_video_frames: u32,
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> Result<StreamStats, String> {
    log::info!("streambeam connecting with pairing payload");
    let session = connect_as_identity(
        &payload,
        &crate::store::client_name(),
        Some(&client_identity_path()),
        video_caps(max_width, max_height, max_fps),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam connect failed: {e}");
        e.to_string()
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
    max_width: u16,
    max_height: u16,
    max_fps: u8,
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
        &crate::store::client_name(),
        &client_identity_path(),
        video_caps(max_width, max_height, max_fps),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam LAN connect failed: {e}");
        e.to_string()
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
    max_width: u16,
    max_height: u16,
    max_fps: u8,
) -> Result<StreamStats, String> {
    log::info!("streambeam reconnecting to saved host at {addr}");
    let session = connect_known(
        host.host_pubkey,
        addr,
        &crate::store::client_name(),
        &client_identity_path(),
        video_caps(max_width, max_height, max_fps),
    )
    .await
    .map_err(|e| {
        log::error!("streambeam reconnect failed: {e}");
        e.to_string()
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
    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::input::set_sender(Some(input_tx));

    let input_task = runtime.spawn(crate::input::pump(input, input_rx));

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
    let (sps, pps) =
        extract_avc_decoder_config(config).ok_or("Host video config is missing H.264 SPS/PPS.")?;
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
    codec
        .start()
        .map_err(|e| codec_err("start", media_err_label(e)))?;

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
    // Once-per-second pipeline funnel, logged to logcat (tag StreamBeam) so a
    // stall can be localized without guessing: wire (datagrams/bytes in) →
    // reassembly (frames completed/abandoned) → feed (fed vs skipped-behind)
    // → render (buffers actually released to the surface). Each stage's
    // number should roughly explain the next; the first big drop-off between
    // adjacent stages is where the bitrate/fps problem lives.
    let mut diag = FunnelDiag::new(asm.stats());
    // Was hardcoded to a flat 33ms (30fps) regardless of the actually
    // negotiated `session.fps` — harmless at 30fps, but wrong (and
    // increasingly so) at any higher rate the host might now be sending,
    // since MediaCodec times output relative to these presentation
    // timestamps.
    let pts_step_us = 1_000_000 / u64::from(session.fps.max(1));
    let mut pts_us: u64 = pts_step_us;
    // How many decodable (non-keyframe) completions we've skipped in a row
    // because we were catching up on a backlog — see the loop body.
    let mut consecutive_skipped = 0u32;
    // Raised from 8 after real-device testing on a fast, clean LAN (RTT
    // <20ms, host reporting 0 lost/congested packets) still showed a
    // `RequestIdr`/`InvalidateRefFrames` every 2-4s at this threshold — each
    // one forces a full NVENC session recreate host-side (`capture.rs` has
    // no live-reconfigure path yet), and that recreate's own brief
    // stall/burst produces exactly the multi-frame-per-batch bursts that
    // trip `behind` detection, requesting another "recovery" that wasn't
    // needed: a self-sustaining cycle capping fps well under 60 with no
    // real loss anywhere. A real, growing backlog still hits even a much
    // higher threshold within a couple of seconds; this just stops short,
    // healthy bursts (e.g. right after a keyframe) from mistakenly counting.
    const IDR_RESYNC_THRESHOLD: u32 = 40;
    // Don't hammer the host with repeated keyframe requests: a forced
    // keyframe is itself a large burst (150-300 KB even at 1080p) that can
    // cause the very backlog it's meant to fix if requested too often —
    // without this, a host that's merely a bit slow (not catastrophically
    // behind) oscillates between huge keyframe bursts and near-stalls instead
    // of settling down.
    const IDR_REQUEST_COOLDOWN: Duration = Duration::from_secs(3);
    let mut last_idr_request: Option<std::time::Instant> = None;

    while stats.video_frames < target {
        diag.rendered += drain_output(&mut codec)?;

        // One `block_on` call for the whole batch, not one per datagram —
        // at the tighter poll rate this loop now runs at (see `TIMEOUT`),
        // calling `block_on` per-datagram (up to 64 times/iteration) added
        // real, measurable overhead entering the async runtime thousands of
        // times a second, showing up as sustained high CPU use and starving
        // other tasks on the same runtime (e.g. the input-forwarding task).
        let batch = runtime.block_on(async {
            let first = tokio::time::timeout(Duration::from_secs(5), session.conn.read_datagram())
                .await
                .map_err(|_| "Timed out waiting for video from host.".to_string())?
                .map_err(|e| format!("Stream read failed: {e}"))?;

            // Greedily grab anything ALREADY sitting in the receive queue too
            // (a near-zero wait; genuinely new arrivals won't show up in time).
            // More than one datagram back-to-back means decode/render fell
            // behind real time since the last iteration — plain FIFO processing
            // of an ever-growing backlog is exactly how you end up watching
            // content from a minute ago with no way to ever catch up.
            let mut batch = vec![first];
            while let Ok(Ok(next)) =
                tokio::time::timeout(Duration::from_millis(2), session.conn.read_datagram()).await
            {
                batch.push(next);
                if batch.len() >= 64 {
                    break; // safety valve against an unbounded drain loop
                }
            }
            Ok::<_, String>(batch)
        })?;
        // A single access unit is FEC-sharded into many ~1200-byte datagrams
        // (see MAX_SHARD_PAYLOAD) — a 15 KB P-frame is a dozen-plus shards
        // that legitimately arrive back-to-back within this batch's 2ms
        // drain window. So `batch.len() > 1` is almost always true and isn't
        // a backlog signal at all; the real signal is how many *complete
        // frames* this batch reassembled.
        diag.datagrams += batch.len() as u64;
        diag.max_batch = diag.max_batch.max(batch.len() as u32);
        let mut completions: Vec<(bool, Vec<u8>)> = Vec::new();
        for datagram in batch.iter() {
            diag.bytes += datagram.len() as u64;
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
                    completions.push((is_keyframe, frame));
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("streambeam frame reassembly failed: {e}");
                }
            }
        }

        // On a fast, low-latency LAN at high fps (this is meant to sustain
        // up to 60fps — ~16.7ms/frame), several frames legitimately
        // finishing in the same ~2ms drain window is common — especially
        // right after a host-side NVENC session recreate (see
        // `IDR_RESYNC_THRESHOLD`'s comment), which briefly stalls then
        // flushes a small burst once it resumes. Treating that as "behind"
        // self-inflicted a resync-request cycle even with zero real network
        // loss. A real, growing backlog keeps piling up far more than a
        // short burst, so require a much larger one to call it in.
        let behind = completions.len() > 5;

        for (is_keyframe, frame) in completions {
            // Always decode a keyframe (it resets the reference chain and is
            // worth showing even a little late). Otherwise, while behind, skip
            // every non-keyframe completion in this batch — including the
            // last one. Feeding a lone P-frame here previously fed the
            // decoder a sample whose reference chain was just broken by all
            // the frames we skipped ahead of it (P-frames predict from the
            // immediately preceding frame, which we never decoded), which
            // MediaCodec still "successfully" decodes into garbage/corrupted
            // output rather than erroring — indistinguishable from a black
            // screen. Better to show nothing until the next keyframe resyncs
            // the reference chain than to render corrupted frames.
            if behind && !is_keyframe {
                consecutive_skipped += 1;
                diag.skipped_behind += 1;
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
            let t_feed = std::time::Instant::now();
            feed_sample(&mut codec, &sample, pts_us, flags)?;
            diag.feed_us += t_feed.elapsed().as_micros() as u64;
            diag.fed += 1;
            pts_us += pts_step_us;
            stats.video_frames = asm.stats().frames_complete;
            diag.rendered += drain_output(&mut codec)?;
            consecutive_skipped = 0;
        }
        diag.maybe_log(asm.stats(), &session.conn);

        // We've been skipping non-keyframes for a while (they depend on a
        // reference chain we're no longer decoding) — ask the host to
        // recover so the picture stops degrading. Only if we're not still
        // cooling down from the last request, so a host that's persistently
        // a little behind doesn't get spammed.
        if consecutive_skipped >= IDR_RESYNC_THRESHOLD {
            consecutive_skipped = 0;
            let cooled_down = last_idr_request
                .map(|t| t.elapsed() >= IDR_REQUEST_COOLDOWN)
                .unwrap_or(true);
            if cooled_down {
                last_idr_request = Some(std::time::Instant::now());
                // Prefer the cheaper, surgical RFI recovery when the
                // reassembler still has a recent-enough reference to
                // invalidate around; fall back to a full keyframe request
                // otherwise (no report at all, or NeedIdr).
                match asm.take_loss_report() {
                    Some(LossReport::Invalidate {
                        first_frame,
                        last_frame,
                    }) => {
                        if let Err(e) = runtime.block_on(request_invalidate_ref_frames(
                            &mut session,
                            first_frame,
                            last_frame,
                        )) {
                            log::warn!(
                                "streambeam catch-up invalidate-ref-frames failed (continuing): {e}"
                            );
                        }
                    }
                    Some(LossReport::NeedIdr) | None => {
                        if let Err(e) = runtime.block_on(request_idr(&mut session)) {
                            log::warn!("streambeam catch-up request_idr failed (continuing): {e}");
                        }
                    }
                }
            }
        }
    }

    codec.stop().ok();
    Ok(stats)
}

fn feed_sample(codec: &mut MediaCodec, data: &[u8], pts_us: u64, flags: u32) -> Result<(), String> {
    // `TIMEOUT` is intentionally short (see its doc comment) so a single
    // dequeue attempt no longer stalls the whole loop — but that also means
    // "no input buffer free yet" is now a routine, momentary condition
    // instead of the rare case it was at the old 30ms timeout. Retry a
    // handful of times (a few ms total) before treating it as a real error.
    let mut input = None;
    for _ in 0..10 {
        match codec
            .dequeue_input_buffer(TIMEOUT)
            .map_err(|e| codec_err("dequeue input", e))?
        {
            DequeuedInputBufferResult::Buffer(buf) => {
                input = Some(buf);
                break;
            }
            DequeuedInputBufferResult::TryAgainLater => continue,
        }
    }
    let Some(mut input_buf) = input else {
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

/// Returns how many output buffers were released to the surface (rendered).
fn drain_output(codec: &mut MediaCodec) -> Result<u32, String> {
    let mut rendered = 0u32;
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
                    rendered += 1;
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
    Ok(rendered)
}

/// Per-second pipeline funnel for logcat — see the doc where it's constructed.
struct FunnelDiag {
    window_start: std::time::Instant,
    datagrams: u64,
    bytes: u64,
    max_batch: u32,
    skipped_behind: u32,
    fed: u32,
    feed_us: u64,
    rendered: u32,
    last_complete: u64,
    last_abandoned: u64,
    last_udp_rx: u64,
    last_frame_rx: u64,
    last_lost: u64,
}

impl FunnelDiag {
    fn new(stats: &streambeam_core::FrameStats) -> Self {
        Self {
            window_start: std::time::Instant::now(),
            datagrams: 0,
            bytes: 0,
            max_batch: 0,
            skipped_behind: 0,
            fed: 0,
            feed_us: 0,
            rendered: 0,
            last_complete: stats.frames_complete,
            last_abandoned: stats.frames_abandoned,
            last_udp_rx: 0,
            last_frame_rx: 0,
            last_lost: 0,
        }
    }

    /// `conn` is queried for QUIC-layer receive counters, to bisect where
    /// datagrams disappear: `udp_rx` counts raw UDP packets reaching this
    /// endpoint, `frame_rx.datagram` counts DATAGRAM frames the QUIC layer
    /// accepted, and `self.datagrams` counts what the app actually read.
    /// udp high / frame low → arriving but not parsing (MTU/coalescing);
    /// frame high / app low → dropped in quinn's receive buffer; all low →
    /// genuinely not arriving over the air.
    fn maybe_log(&mut self, stats: &streambeam_core::FrameStats, conn: &quinn::Connection) {
        let secs = self.window_start.elapsed().as_secs_f64();
        if secs < 1.0 {
            return;
        }
        let complete = stats.frames_complete - self.last_complete;
        let abandoned = stats.frames_abandoned - self.last_abandoned;
        let qs = conn.stats();
        log::info!(
            "streambeam funnel: wire {} dgrams {:.0} kbps | quic +{} udp pkts +{} dgram frames, rtt {} ms, +{} lost | asm {complete} complete {abandoned} abandoned | feed {} fed ({} ms avg) {} skipped-behind | render {} ({:.1} fps) | max batch {}",
            self.datagrams,
            (self.bytes as f64 * 8.0 / 1000.0) / secs,
            qs.udp_rx.datagrams.saturating_sub(self.last_udp_rx),
            qs.frame_rx.datagram.saturating_sub(self.last_frame_rx),
            qs.path.rtt.as_millis(),
            qs.path.lost_packets.saturating_sub(self.last_lost),
            self.fed,
            if self.fed > 0 {
                self.feed_us / u64::from(self.fed) / 1000
            } else {
                0
            },
            self.skipped_behind,
            self.rendered,
            f64::from(self.rendered) / secs,
            self.max_batch,
        );
        self.last_udp_rx = qs.udp_rx.datagrams;
        self.last_frame_rx = qs.frame_rx.datagram;
        self.last_lost = qs.path.lost_packets;
        self.last_complete = stats.frames_complete;
        self.last_abandoned = stats.frames_abandoned;
        self.datagrams = 0;
        self.bytes = 0;
        self.max_batch = 0;
        self.skipped_behind = 0;
        self.fed = 0;
        self.feed_us = 0;
        self.rendered = 0;
        self.window_start = std::time::Instant::now();
    }
}
