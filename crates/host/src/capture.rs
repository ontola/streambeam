// This module is Win32/COM FFI glue; each `unsafe fn` body is deliberately one
// big unsafe region rather than nesting `unsafe {}` around every call.
#![allow(unsafe_op_in_unsafe_fn)]

//! Real desktop capture (DXGI Desktop Duplication) encoded to H.264.
//!
//! Hardware path: direct NVIDIA NVENC (see `nvenc.rs`) — a real `ID3D11Texture2D`
//! registered once, the captured frame `CopyResource`'d into it every tick, no
//! CPU touching pixel data. This replaced an earlier attempt that went through
//! Media Foundation's generic hardware-encoder-MFT wrapper: that got NVENC
//! producing frames, but hit an intermittent, unrecoverable silent freeze after
//! a variable number of frames (confirmed via a live stack trace: the capture
//! thread was parked on a normal wait, not stuck inside any MF/driver call).
//! Sunshine and RustDesk both bypass MF for NVIDIA entirely — see `nvenc.rs`'s
//! module doc for the full writeup. When no NVIDIA encoder is available (or it
//! fails to initialize), this falls back to the built-in Media Foundation
//! *software* H.264 encoder, run synchronously (that contract has none of the
//! async-event failure modes above).
//!
//! All COM lives on one dedicated thread (COM objects have thread affinity and
//! tokio moves tasks between workers). [`DesktopEncoder`] talks to that thread
//! over channels and implements [`VideoEncoder`], so `server.rs` can swap it in
//! for the mock without caring how frames are produced.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_RESOURCE_MISC_FLAG, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter,
    IDXGIDevice, IDXGIOutput1, IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};

use crate::encode::{Codec, EncodeError, EncodedFrame, VideoEncoder, default_bitrate_bps};
use crate::nvenc::NvencEncoder;
use crate::scale::GpuScaler;

// The built-in software H.264 encoder MFT (synchronous — no async event plumbing).
const CLSID_CMSH264ENCODER: GUID = GUID::from_u128(0x6ca5_0344_051a_4ded_9779_a433_0516_5e35);
// CODECAPI properties (defined here to avoid feature-gated re-exports).
const CODECAPI_AVLOWLATENCYMODE: GUID = GUID::from_u128(0x9c27_891a_ed7a_40e1_88e8_b227_27a0_24ee);
const CODECAPI_FORCEKEYFRAME: GUID = GUID::from_u128(0x398c_1b98_8353_475a_9ef2_8f26_5d26_0345);
const CODECAPI_GOPSIZE: GUID = GUID::from_u128(0x95f3_1b26_95a4_41aa_9303_246a_7fc6_eef1);
const CODECAPI_RATECONTROL: GUID = GUID::from_u128(0x1c06_08e9_370c_4710_8a58_cb61_81c4_2423);
const CODECAPI_MEANBITRATE: GUID = GUID::from_u128(0xf722_2374_2144_4815_b550_a37f_8e12_ee52);

enum Request {
    Next,
    ForceIdr,
    /// Nudge only — the actual target bitrate lives in `pending_bitrate`
    /// (mirrors `ForceIdr`'s `force_idr: Arc<AtomicBool>` pattern), so a
    /// request already queued behind others in the channel doesn't carry a
    /// stale value by the time it's handled.
    SetBitrate,
    /// Nudge only — the timestamps live in `pending_invalidate` (same
    /// staleness reasoning as `SetBitrate`; a newer invalidate supersedes an
    /// unconsumed older one, mirroring `StreamControl::request_invalidate`).
    Invalidate,
}

/// Video encoder backed by a background capture+encode thread.
pub struct DesktopEncoder {
    width: u32,
    height: u32,
    /// Shared with `Pipeline` on the capture thread — `codec_config()` isn't
    /// known until the first real keyframe is encoded, which happens well
    /// after `new()` returns (see `Pipeline::capture_and_encode`), so a
    /// snapshot taken at construction time is always `None` and never
    /// updates. This cell is the live view `server.rs`'s poll loop needs.
    codec_config: Arc<Mutex<Option<Vec<u8>>>>,
    req_tx: Sender<Request>,
    frame_rx: Receiver<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
    pending_bitrate: Arc<Mutex<Option<u32>>>,
    pending_invalidate: Arc<Mutex<Option<Vec<u64>>>>,
    /// The codec actually driving the encoder — may not match what was
    /// requested (see `codec()`).
    codec: Codec,
    _thread: JoinHandle<()>,
}

impl DesktopEncoder {
    /// `max_encode_dim` caps the encoded video's longest side (native capture
    /// resolution is unaffected) — see the field doc on `Pipeline` for why.
    /// `wanted_codec` is a request, not a guarantee — `codec()` reports
    /// what actually ended up encoding (see `Pipeline::new`'s AV1-then-H.264
    /// NVENC fallback).
    pub fn new(
        fps: u32,
        max_encode_dim: u32,
        wanted_codec: Codec,
        wants_hdr: bool,
    ) -> Result<Self, String> {
        let (init_tx, init_rx) =
            std::sync::mpsc::channel::<Result<(u32, u32, Codec), String>>();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<Option<EncodedFrame>>(4);
        let force_idr = Arc::new(AtomicBool::new(false));
        let thread_force = force_idr.clone();
        let pending_bitrate = Arc::new(Mutex::new(None));
        let thread_bitrate = pending_bitrate.clone();
        let pending_invalidate = Arc::new(Mutex::new(None));
        let thread_invalidate = pending_invalidate.clone();
        let codec_config = Arc::new(Mutex::new(None));
        let thread_codec_config = codec_config.clone();

        let thread = std::thread::Builder::new()
            .name("streambeam-capture".into())
            .spawn(move || {
                run_pipeline(
                    fps,
                    max_encode_dim,
                    wanted_codec,
                    init_tx,
                    req_rx,
                    frame_tx,
                    thread_force,
                    thread_bitrate,
                    thread_invalidate,
                    thread_codec_config,
                )
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match init_rx.recv() {
            Ok(Ok((width, height, codec))) => Ok(Self {
                width,
                height,
                codec_config,
                req_tx,
                frame_rx,
                force_idr,
                pending_bitrate,
                pending_invalidate,
                codec,
                _thread: thread,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("capture thread exited during init".into()),
        }
    }
}

impl VideoEncoder for DesktopEncoder {
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        if self.req_tx.send(Request::Next).is_err() {
            return Err(EncodeError::Other("capture thread gone".into()));
        }
        match self.frame_rx.recv() {
            Ok(frame) => Ok(frame),
            Err(_) => Err(EncodeError::Other("capture thread gone".into())),
        }
    }

    fn force_idr(&mut self) {
        self.force_idr.store(true, Ordering::Relaxed);
        let _ = self.req_tx.send(Request::ForceIdr);
    }

    fn set_bitrate(&mut self, kbps: u32) {
        *self.pending_bitrate.lock().unwrap() = Some(kbps.saturating_mul(1000));
        let _ = self.req_tx.send(Request::SetBitrate);
    }

    fn invalidate_ref_timestamps(&mut self, timestamps: &[u64]) {
        *self.pending_invalidate.lock().unwrap() = Some(timestamps.to_vec());
        let _ = self.req_tx.send(Request::Invalidate);
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.lock().unwrap().clone()
    }

    fn codec(&self) -> Codec {
        self.codec
    }
}

/// Body of the capture thread: init, report dims, then serve encode requests.
fn run_pipeline(
    fps: u32,
    max_encode_dim: u32,
    wanted_codec: Codec,
    init_tx: Sender<Result<(u32, u32, Codec), String>>,
    req_rx: Receiver<Request>,
    frame_tx: SyncSender<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
    pending_bitrate: Arc<Mutex<Option<u32>>>,
    pending_invalidate: Arc<Mutex<Option<Vec<u64>>>>,
    codec_config: Arc<Mutex<Option<Vec<u8>>>>,
) {
    // Attach this capture thread to the current input desktop *before*
    // creating the DXGI duplication — under the SYSTEM service this is what
    // lets capture see the lock/login/UAC (Winlogon) desktop; in a normal
    // user session it's a no-op onto the desktop we're already on. Re-run on
    // every ACCESS_LOST recovery (see `capture_frame`) since a desktop switch
    // is exactly what raises that error.
    crate::desktop_win::input_desktop().follow();
    let mut pipe = match Pipeline::new(fps, max_encode_dim, wanted_codec) {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    let codec = pipe.codec();
    if init_tx.send(Ok((pipe.width, pipe.height, codec))).is_err() {
        return;
    }

    while let Ok(req) = req_rx.recv() {
        match req {
            Request::ForceIdr | Request::SetBitrate | Request::Invalidate => {
                // The flags/values are all also read in the Next arm below;
                // nothing to do here beyond having woken up a thread that
                // might otherwise be parked waiting on the next real tick.
            }
            Request::Next => {
                let want_idr = force_idr.swap(false, Ordering::Relaxed);
                if let Some(bps) = pending_bitrate.lock().unwrap().take() {
                    pipe.set_bitrate(bps);
                }
                match pending_invalidate.lock().unwrap().take() {
                    // A pending forced IDR supersedes invalidation — the
                    // keyframe it forces references nothing the invalidate
                    // was trying to avoid (same precedence `stream_video`
                    // applies between the two control signals).
                    Some(_) if want_idr => {}
                    // Safety: same dedicated thread that owns the NVENC
                    // session and drives `capture_and_encode`.
                    Some(ts) => unsafe { pipe.invalidate_refs(&ts) },
                    None => {}
                }
                let frame = pipe.capture_and_encode(want_idr).unwrap_or_else(|e| {
                    crate::logging::log(&format!("capture/encode error: {e}"));
                    None
                });
                if pipe.codec_config.is_some() {
                    let mut shared = codec_config.lock().unwrap();
                    if shared.is_none() {
                        *shared = pipe.codec_config.clone();
                    }
                }
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
        }
    }
}

/// Which encoder is actually driving `Pipeline`, decided once at startup by
/// whether NVENC initialized successfully.
enum EncoderBackend {
    /// Direct NVENC (see `nvenc.rs`). `encode_texture` is the D3D11 texture
    /// registered with the encoder; each tick's captured frame is
    /// `CopyResource`'d straight in (GPU-to-GPU) when capture and encode
    /// resolutions match, or run through `scaler` (also GPU-only, a
    /// pixel-shader blit — see `scale.rs`) when they don't.
    Nvenc {
        encoder: NvencEncoder,
        encode_texture: ID3D11Texture2D,
        scaler: Option<GpuScaler>,
    },
    /// Fallback for non-NVIDIA machines: the built-in Media Foundation
    /// software H.264 encoder, fed NV12 converted from the captured BGRA.
    Software {
        transform: IMFTransform,
        nv12: Vec<u8>,
    },
}

struct Pipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// CPU-readable copy of the captured frame — only used by the software
    /// encoder fallback path (NVENC never touches the CPU).
    staging: ID3D11Texture2D,
    backend: EncoderBackend,
    /// Native desktop resolution — the size of `staging`.
    capture_width: u32,
    capture_height: u32,
    /// Encoded resolution — `capture_{width,height}` downscaled to
    /// `max_encode_dim`, preserving aspect ratio. This is what the encoder
    /// and `EncodedFrame` use.
    width: u32,
    height: u32,
    fps: u32,
    /// Current NVENC target bitrate. Mutable at runtime (see
    /// `Pipeline::set_bitrate`/`server.rs`'s adaptive controller) — applied
    /// via `NvencEncoder::reconfigure` (live, no session teardown).
    bitrate_bps: u32,
    /// Set by `set_bitrate` when `bitrate_bps` changed since the last
    /// reconfigure; cleared once a reconfigure has actually applied it.
    bitrate_dirty: bool,
    /// A forced-IDR request that arrived while a reconfigure was already
    /// being coalesced (see `last_reconfigure`) — kept pending rather than
    /// dropped so a client's real recovery request always eventually gets a
    /// fresh keyframe, just possibly a little later than the call that
    /// requested it.
    pending_force_idr: bool,
    /// When the last live reconfigure happened, for `encode()`'s
    /// reconfigure-coalescing window — see its comment.
    last_reconfigure: Option<std::time::Instant>,
    codec_config: Option<Vec<u8>>,
    have_last: bool,
    frame_index: i64,
    // Diagnostics: logged periodically so a black/frozen stream is debuggable
    // from host.log even in a release build with no console attached.
    stat_calls: u32,
    stat_acquired: u32,
    stat_timeout: u32,
    stat_access_lost: u32,
    stat_produced: u32,
    /// Consecutive `ACCESS_LOST` recreate attempts with no successful frame
    /// in between. Recreating the duplication interface is the right move
    /// for a real transient loss (display mode change, GPU driver reset),
    /// but it can't do anything about the two conditions that cause a *long*
    /// run of them: the display is powered off (idle timeout) or the
    /// session is locked — Desktop Duplication categorically doesn't work
    /// in either state, no matter how many times it's recreated. Past a
    /// threshold, back off and say so once instead of retrying every ~33ms
    /// forever and filling host.log with near-identical lines.
    consecutive_access_lost: u32,
    /// Windows.Graphics.Capture fallback, started only once Desktop
    /// Duplication has proven it can't see the screen (a game holding
    /// exclusive presentation — see `wgc.rs`). `None` while duplication works,
    /// which is the normal case and the cheaper path.
    wgc: Option<crate::wgc::WgcCapture>,
    /// When the fallback was last started, so a WGC that fails immediately
    /// can't be restarted in a tight loop. Learned the hard way: a
    /// misclassified "pool empty" made it start-and-revert 708 times in one
    /// session, each cycle logging two lines and rebuilding a frame pool.
    wgc_last_start: Option<std::time::Instant>,
    /// Access-lost *rate*, tracked because a consecutive-run trigger alone
    /// misses the case that actually hurts. A game produces churn — lose the
    /// duplication, recreate it, get one good frame, lose it again — and every
    /// successful acquire resets the consecutive counter, so a run of 4 is
    /// rare even while thousands accumulate. Meanwhile each recreate costs
    /// ~40ms of capture time, measured live at 4072 access-losts and 20fps
    /// against a 120fps target. Churn is the signal; a run is just its
    /// tidiest form.
    access_lost_window: Option<std::time::Instant>,
    access_lost_in_window: u32,
    /// SDR white level the scaler's tone-map was compiled with, to compare
    /// against what the display reports later — see `note_sdr_white`.
    scaler_sdr_white: f32,
    warned_sdr_white: bool,
    /// Rolling per-report-window totals (reset every 90 calls alongside the
    /// counters above) for pinpointing where per-frame time actually goes.
    stat_capture_us: u64,
    stat_encode_us: u64,
}

/// Consecutive `ACCESS_LOST` recreate attempts before giving up on Desktop
/// Duplication and switching to WGC. Deliberately the same point at which the
/// old code started backing off to a 2s retry: by then recreation has failed
/// three times, which no transient display-mode change survives.
const WGC_FALLBACK_AFTER_ACCESS_LOST: u32 = 4;

/// Access-losses within [`ACCESS_LOST_WINDOW`] that count as churn rather than
/// a transient loss. A real display-mode change costs a handful; this many
/// means something is repeatedly taking the desktop away, and each recreate is
/// ~40ms of capture time that the frame rate pays for.
const ACCESS_LOST_CHURN: u32 = 30;
const ACCESS_LOST_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

fn ctx<T>(r: windows::core::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

impl Pipeline {
    /// `max_encode_dim` caps the longest encoded side. The FEC layer addresses
    /// shards with a `u16` count (see `MAX_DATA_SHARDS` in `streambeam_proto`),
    /// and a very large keyframe (native 4K, say) can still exceed it — a
    /// phone screen has no use for that much detail anyway.
    fn new(
        fps: u32,
        max_encode_dim: u32,
        wanted_codec: Codec,
        wants_hdr: bool,
    ) -> Result<Self, String> {
        unsafe {
            // MTA so the objects can be used from this thread freely.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            ctx(MFStartup(MF_VERSION, MFSTARTUP_FULL), "MFStartup")?;

            let (device, context) = create_d3d_device()?;
            let (mut duplication, capture_width, capture_height, sdr_white_nits) =
                create_duplication(&device, true)?;
            let staging = create_staging(&device, capture_width, capture_height)?;
            let (width, height) = scaled_dimensions(capture_width, capture_height, max_encode_dim);
            if (width, height) != (capture_width, capture_height) {
                crate::logging::log(&format!(
                    "downscaling capture {capture_width}x{capture_height} -> {width}x{height} to stay within FEC shard limits"
                ));
            }

            let bitrate_bps = default_bitrate_bps(width, height, fps);
            // Whether capture is actually handing us FP16 scRGB. Passthrough
            // needs the real HDR signal — with `STREAMBEAM_HDR_CAPTURE=dwm`
            // this is false, because DWM has already flattened it to SDR and
            // there is nothing left to pass through.
            let hdr_capture = desktop_is_hdr(&device, true);
            let try_nvenc = |codec: Codec| -> Result<(NvencEncoder, ID3D11Texture2D, Option<GpuScaler>), String> {
                // HDR10 passthrough (docs/HDR.md): only when asked for, only on
                // an HDR desktop, and only for AV1. Anything else keeps the
                // tone-mapped SDR path, which stays the default.
                let hdr10 = (wants_hdr || hdr10_forced()) && hdr_capture && codec == Codec::Av1;
                let tex = create_encode_texture(&device, width, height, hdr10)?;
                let encoder =
                    NvencEncoder::new(&device, &tex, width, height, fps, bitrate_bps, codec, hdr10)?;
                if hdr10 {
                    crate::logging::log(
                        "HDR10 passthrough: encoding 10-bit PQ / BT.2020, no tone-map",
                    );
                }
                // Always present (not just when downscaling): on an HDR
                // desktop the captured frame is FP16 scRGB and must go
                // through the tone-map shader even at 1:1 — a same-size
                // bilinear blit samples exact texel centers, so the SDR
                // case loses nothing to it either.
                let scaler = Some(GpuScaler::new(
                    &device,
                    &context,
                    width,
                    height,
                    &tex,
                    sdr_white_nits,
                    hdr10,
                )?);
                Ok((encoder, tex, scaler))
            };
            // AV1 support is GPU-generation-gated (Ada Lovelace/RTX 40-series
            // and later) in a way plain H.264 NVENC isn't, so a client asking
            // for AV1 on an older NVIDIA GPU is a real, expected case, not
            // just "no NVENC at all" — worth a distinct retry at H.264 on
            // NVENC before falling all the way to the software encoder.
            let nvenc_attempt = try_nvenc(wanted_codec).or_else(|e| {
                if wanted_codec == Codec::Av1 {
                    crate::logging::log(&format!(
                        "AV1 NVENC unavailable ({e}) — retrying with H.264 NVENC"
                    ));
                    try_nvenc(Codec::H264)
                } else {
                    Err(e)
                }
            });
            let backend = match nvenc_attempt {
                Ok((encoder, encode_texture, scaler)) => {
                    crate::logging::log(&format!(
                        "using direct NVENC hardware encoder ({:?})",
                        encoder.codec()
                    ));
                    EncoderBackend::Nvenc {
                        encoder,
                        encode_texture,
                        scaler,
                    }
                }
                Err(e) => {
                    crate::logging::log(&format!(
                        "NVENC unavailable ({e}) — falling back to Media Foundation software encoder (H.264)"
                    ));
                    // The software path reads captured bytes on the CPU as
                    // 8-bit BGRA (`bgra_to_nv12`) — recreate the duplication
                    // SDR-only so an HDR desktop goes back through DWM's own
                    // conversion instead of handing this path FP16 frames it
                    // can't parse.
                    let (dup, _, _, _) = create_duplication(&device, false)?;
                    duplication = dup;
                    let transform = create_software_encoder(width, height, fps)?;
                    EncoderBackend::Software {
                        transform,
                        nv12: vec![0u8; (width * height * 3 / 2) as usize],
                    }
                }
            };
            let codec_config = None;

            Ok(Self {
                device,
                context,
                duplication,
                staging,
                backend,
                capture_width,
                capture_height,
                width,
                height,
                fps,
                bitrate_bps,
                bitrate_dirty: false,
                pending_force_idr: false,
                last_reconfigure: None,
                codec_config,
                have_last: false,
                frame_index: 0,
                stat_calls: 0,
                stat_acquired: 0,
                stat_timeout: 0,
                stat_access_lost: 0,
                consecutive_access_lost: 0,
                wgc: None,
                wgc_last_start: None,
                access_lost_window: None,
                access_lost_in_window: 0,
                scaler_sdr_white: sdr_white_nits,
                warned_sdr_white: false,
                stat_produced: 0,
                stat_capture_us: 0,
                stat_encode_us: 0,
            })
        }
    }

    /// Grab the newest desktop frame (or reuse the last if unchanged), encode it,
    /// and return the resulting access unit (None while the encoder primes).
    fn capture_and_encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        self.stat_calls += 1;

        let t_capture = std::time::Instant::now();
        let captured = unsafe { self.capture_frame() };
        self.stat_capture_us += t_capture.elapsed().as_micros() as u64;

        let result = captured.and_then(|()| {
            let t_encode = std::time::Instant::now();
            let r = unsafe { self.encode(force_idr) };
            self.stat_encode_us += t_encode.elapsed().as_micros() as u64;
            r
        });

        // Neither backend reliably exposes the codec config ahead of the
        // first real frame (the software MFT's `MF_MT_MPEG_SEQUENCE_HEADER`
        // requires a frame to have gone through; NVENC's preset config
        // doesn't carry it either) — pull it straight out of the first
        // keyframe's own bytes instead, which works regardless of encoder
        // vendor. Format depends on the active codec: H.264's is SPS/PPS
        // NALs, AV1's is the sequence header OBU (self-contained, no
        // separate assembly needed).
        if self.codec_config.is_none() {
            if let Ok(Some(frame)) = &result {
                if frame.keyframe {
                    self.codec_config = match self.codec() {
                        Codec::H264 => streambeam_core::extract_avc_decoder_config(&frame.data)
                            .map(|(sps, pps)| {
                                let mut config = streambeam_core::nal_with_start_code(&sps);
                                config.extend(streambeam_core::nal_with_start_code(&pps));
                                config
                            }),
                        Codec::Av1 => streambeam_core::av1_extract_sequence_header(&frame.data),
                    };
                }
            }
        }

        if let Ok(Some(_)) = result {
            self.stat_produced += 1;
        }
        // Roughly every 3s at 30fps — enough to catch a stuck pipeline quickly
        // without flooding host.log.
        const REPORT_WINDOW: u64 = 90;
        if self.stat_calls % REPORT_WINDOW as u32 == 0 {
            crate::logging::log(&format!(
                "capture stats: {} calls — {} acquired, {} timeout, {} access-lost, {} AUs produced, avg capture={} ms, avg encode={} ms",
                self.stat_calls,
                self.stat_acquired,
                self.stat_timeout,
                self.stat_access_lost,
                self.stat_produced,
                self.stat_capture_us / REPORT_WINDOW / 1000,
                self.stat_encode_us / REPORT_WINDOW / 1000,
            ));
            self.stat_capture_us = 0;
            self.stat_encode_us = 0;
        }
        result
    }

    /// Acquire the newest desktop frame and land it wherever the active
    /// backend needs it: `encode_texture` (GPU-to-GPU, or CPU-resized only if
    /// downscaling) for NVENC, `nv12` for the software encoder.
    unsafe fn capture_frame(&mut self) -> Result<(), String> {
        if self.wgc.is_some() {
            return self.capture_frame_wgc();
        }
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // Wait at most one frame interval for a *changed* desktop frame —
        // the timeout path below re-encodes the previous frame, so this
        // bound sets the encode cadence on a static screen. The old flat
        // 100ms collapsed an idle desktop to ~10fps while NVENC's CBR
        // controller still budgeted bytes for the negotiated fps (e.g.
        // 120), so the frame that finally carried a change (a cursor move)
        // got ~1/12th of its real byte budget and was visibly crushed —
        // near-black gradients pumping between grey and black as quality
        // then crept back on the static frames. Encoding duplicates at the
        // real cadence keeps the rate controller's per-frame budget honest
        // and lets VBV headroom accumulate for the frames that matter
        // (duplicates cost a few hundred bytes each), and makes
        // `gop_len = fps*3` actually mean ~3s of wall clock.
        let timeout_ms = (1000 / self.fps.max(1)).max(1);
        match self
            .duplication
            .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        {
            Ok(()) => {
                self.stat_acquired += 1;
                self.consecutive_access_lost = 0;
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                self.stat_timeout += 1;
                self.consecutive_access_lost = 0;
                // No change since last frame; reuse whatever the backend already
                // holds (or, the very first time, a black software frame — the
                // NVENC texture's undefined initial content is one frame's worth
                // of harmless garbage, replaced as soon as a real frame lands).
                if !self.have_last {
                    if let EncoderBackend::Software { nv12, .. } = &mut self.backend {
                        nv12.iter_mut().enumerate().for_each(|(i, b)| {
                            *b = if i < (self.width * self.height) as usize {
                                16
                            } else {
                                128
                            };
                        });
                    }
                    self.have_last = true;
                }
                return Ok(());
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.stat_access_lost += 1;
                self.consecutive_access_lost += 1;
                let churning = self.note_access_lost_rate();
                // First few: log and retry immediately, matching the pacing
                // of a real transient loss (display mode change) which
                // typically clears within one or two frames.
                if self.consecutive_access_lost <= 3 {
                    crate::logging::log(&format!("duplication access lost, recreating: {e}"));
                } else if self.consecutive_access_lost == 4 {
                    // Recreating hasn't helped 3 times in a row — this isn't a
                    // transient loss anymore. Naming only the locked/display-off
                    // pair here actively misled a debugging session (2026-08-17:
                    // launching a game produced exactly this, on an unlocked box
                    // with the display on), so list what really causes it. Then
                    // back off instead of spamming host.log every ~33ms.
                    crate::logging::log(&format!(
                        "duplication access lost, {} consecutive failures — causes, \
                         most→least likely: a full-screen app took exclusive \
                         presentation (game in exclusive fullscreen / independent \
                         flip), the display changed mode or SDR↔HDR, the session is \
                         locked, or the display is powered off. Only the last two are \
                         about the session; a game can do this with the box wide \
                         awake. Will keep retrying quietly every 2s until it \
                         recovers ({e})",
                        self.consecutive_access_lost
                    ));
                } else if self.consecutive_access_lost % 60 == 0 {
                    crate::logging::log(&format!(
                        "duplication access lost, still failing after {} attempts — \
                         a full-screen app may hold exclusive presentation; also \
                         check the display is on and the session unlocked",
                        self.consecutive_access_lost
                    ));
                }
                let allow_hdr = matches!(self.backend, EncoderBackend::Nvenc { .. });
                // ACCESS_LOST is exactly what a desktop switch (lock, login,
                // UAC, or unlock) raises — re-attach this thread to the new
                // input desktop before recreating the duplication so a SYSTEM
                // worker follows onto Winlogon (no-op in a user session).
                crate::desktop_win::input_desktop().follow();
                match create_duplication(&self.device, allow_hdr) {
                    Ok((dup, _, _, nits)) => {
                        self.duplication = dup;
                        self.note_sdr_white(nits);
                    }
                    // Recreation itself can fail outright while the display
                    // is off (no output to enumerate) — that's the same
                    // condition, not a new error; keep backing off instead
                    // of propagating it as a hard stream error.
                    Err(_) => {}
                }
                // Recreation has failed enough times to rule out a transient
                // loss. WGC composites through a different path and keeps
                // working when a game holds the display, so try it before
                // settling into the 2s retry that used to be the whole
                // recovery strategy (and that showed the viewer a frozen
                // picture until their media timeout fired).
                if (self.consecutive_access_lost >= WGC_FALLBACK_AFTER_ACCESS_LOST || churning)
                    && self.start_wgc_fallback(desktop_is_hdr(&self.device, allow_hdr))
                {
                    return Ok(());
                }
                if self.consecutive_access_lost > 3 {
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                }
                return Ok(());
            }
            Err(e) => return Err(format!("AcquireNextFrame: {e}")),
        }

        let result = (|| -> Result<(), String> {
            let resource = resource.ok_or("duplication returned no resource")?;
            let texture: ID3D11Texture2D = ctx(resource.cast(), "resource.cast texture")?;
            self.consume_texture(&texture)
        })();

        let _ = self.duplication.ReleaseFrame();
        result
    }

    /// Land one captured texture wherever the active backend needs it. Shared
    /// by both capture sources — duplication and WGC hand back the same thing
    /// (a texture on our device, BGRA8 or FP16 scRGB), so everything
    /// downstream of here is identical.
    unsafe fn consume_texture(&mut self, texture: &ID3D11Texture2D) -> Result<(), String> {
        match &self.backend {
            EncoderBackend::Nvenc {
                encode_texture,
                scaler,
                ..
            } => match scaler {
                Some(scaler) => scaler.scale(texture)?,
                None => self.context.CopyResource(encode_texture, texture),
            },
            EncoderBackend::Software { .. } => {
                self.context.CopyResource(&self.staging, texture);
            }
        }

        if let EncoderBackend::Software { nv12, .. } = &mut self.backend {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx(
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)),
                "Map staging",
            )?;
            let stride = mapped.RowPitch as usize;
            let src = std::slice::from_raw_parts(
                mapped.pData as *const u8,
                stride * self.capture_height as usize,
            );
            bgra_to_nv12(
                src,
                stride,
                self.capture_width,
                self.capture_height,
                self.width,
                self.height,
                nv12,
            );
            self.context.Unmap(&self.staging, 0);
        }

        self.have_last = true;
        Ok(())
    }

    /// Capture via the WGC fallback. Mirrors the duplication path's contract:
    /// `Ok(())` with nothing new means "re-encode the previous frame".
    unsafe fn capture_frame_wgc(&mut self) -> Result<(), String> {
        let (texture, resized, content) = {
            let wgc = match self.wgc.as_mut() {
                Some(wgc) => wgc,
                None => return Ok(()),
            };
            match wgc.try_next_frame() {
                Ok(Some(frame)) => (
                    frame.texture().clone(),
                    frame.resized,
                    (frame.content_width, frame.content_height),
                ),
                Ok(None) => {
                    // No new frame this tick — same as duplication's timeout.
                    self.stat_timeout += 1;
                    return Ok(());
                }
                Err(e) => {
                    // The pool or session died (monitor unplugged, mode
                    // change). Drop back to duplication rather than spinning:
                    // whatever broke WGC may well have restored duplication.
                    crate::logging::log(&format!(
                        "WGC capture failed ({e}) — reverting to Desktop Duplication"
                    ));
                    self.wgc = None;
                    self.consecutive_access_lost = 0;
                    return Ok(());
                }
            }
        };

        self.stat_acquired += 1;
        let result = self.consume_texture(&texture);
        // Rebuild *after* consuming: this frame is still the old pool size, so
        // it's the last usable one at that geometry. Same limitation the
        // duplication path has on a mode change — the encoder keeps its
        // original dimensions and the scaler resamples; without a scaler
        // (capture size == encode size) a changed desktop resolution needs a
        // full pipeline rebuild, which `server.rs` does on the next session.
        if resized {
            if let Some(wgc) = self.wgc.as_mut() {
                let (w, h) = content;
                if let Err(e) = unsafe { wgc.resize(w, h) } {
                    crate::logging::log(&format!("WGC: pool rebuild failed ({e})"));
                }
            }
        }
        result
    }

    /// Try to replace a blind duplication with WGC. Returns whether the
    /// fallback is now active; failure leaves duplication in place (still
    /// retrying), because a broken fallback must not be worse than no
    /// fallback.
    /// The tone-map's SDR-white divisor is compiled into the scaler at pipeline
    /// build and never changes. That was documented as harmless ("only if the
    /// user moves the SDR content brightness slider"), but a display flipping
    /// SDR↔HDR mid-session changes it too — and that flip is exactly what a
    /// game does. Built while the desktop was SDR, the scaler holds the 200-nit
    /// default; the HDR desktop it then has to tone-map reports 228. Say so,
    /// once, rather than silently tone-mapping with the wrong constant.
    fn note_sdr_white(&mut self, nits: f32) {
        if self.warned_sdr_white || (nits - self.scaler_sdr_white).abs() < 1.0 {
            return;
        }
        self.warned_sdr_white = true;
        crate::logging::log(&format!(
            "HDR tone-map is using a stale SDR white level: scaler was built at \
             {:.0} nits, the display now reports {:.0}. Colours will be off by \
             that ratio until the pipeline is rebuilt (reconnect the client).",
            self.scaler_sdr_white, nits
        ));
    }

    /// Record one access-loss and report whether they're arriving fast enough
    /// to count as churn. Rolling window, reset once it expires — a slow trickle
    /// (one every few minutes, e.g. a monitor waking) never accumulates.
    fn note_access_lost_rate(&mut self) -> bool {
        let now = std::time::Instant::now();
        match self.access_lost_window {
            Some(start) if now.duration_since(start) < ACCESS_LOST_WINDOW => {
                self.access_lost_in_window += 1;
            }
            _ => {
                self.access_lost_window = Some(now);
                self.access_lost_in_window = 1;
            }
        }
        self.access_lost_in_window >= ACCESS_LOST_CHURN
    }

    unsafe fn start_wgc_fallback(&mut self, hdr: bool) -> bool {
        if self.wgc.is_some() {
            return true;
        }
        /// Minimum gap between fallback attempts.
        const RETRY_GAP: std::time::Duration = std::time::Duration::from_secs(10);
        if let Some(last) = self.wgc_last_start
            && last.elapsed() < RETRY_GAP
        {
            return false;
        }
        self.wgc_last_start = Some(std::time::Instant::now());
        match crate::wgc::WgcCapture::new(&self.device, hdr) {
            Ok(wgc) => {
                crate::logging::log(
                    "Desktop Duplication is blind (a full-screen app most likely holds \
                     exclusive presentation) — switching capture to Windows.Graphics.Capture",
                );
                self.wgc = Some(wgc);
                true
            }
            Err(e) => {
                crate::logging::log(&format!(
                    "WGC fallback unavailable ({e}) — staying on Desktop Duplication"
                ));
                false
            }
        }
    }

    /// Queue a new target bitrate — applied on the next `encode()` call via
    /// the same live-reconfigure mechanism `force_idr` uses (see its comment
    /// in `encode()`). Called from `server.rs`'s adaptive bitrate controller
    /// and/or a client's explicit `SetBitrate` request.
    fn set_bitrate(&mut self, bps: u32) {
        if bps != self.bitrate_bps {
            self.bitrate_bps = bps;
            self.bitrate_dirty = true;
        }
    }

    /// Cheap loss recovery: drop the named reference frames from the
    /// encoder's DPB (`NvencEncoder::invalidate_ref_frames`) so the next
    /// frame is an ordinary P-frame against an older still-valid reference —
    /// no keyframe burst, no session recreate. Any failure, or a backend
    /// without reference invalidation, falls back to the pending-IDR path,
    /// which always recovers (via a full session recreate in `encode()`),
    /// just not cheaply.
    unsafe fn invalidate_refs(&mut self, timestamps: &[u64]) {
        match &mut self.backend {
            EncoderBackend::Nvenc { encoder, .. } => {
                match encoder.invalidate_ref_frames(timestamps) {
                    Ok(()) => crate::logging::log(&format!(
                        "NVENC: invalidated {} reference frame(s)",
                        timestamps.len()
                    )),
                    Err(e) => {
                        crate::logging::log(&format!(
                            "NVENC: ref invalidation failed, falling back to forced IDR: {e}"
                        ));
                        self.pending_force_idr = true;
                    }
                }
            }
            EncoderBackend::Software { .. } => self.pending_force_idr = true,
        }
    }

    /// The codec actually in use — the software fallback is always H.264;
    /// NVENC reports whatever it actually initialized with (see `new`'s
    /// AV1-then-H.264 retry).
    fn codec(&self) -> Codec {
        match &self.backend {
            EncoderBackend::Nvenc { encoder, .. } => encoder.codec(),
            EncoderBackend::Software { .. } => Codec::H264,
        }
    }

    unsafe fn encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        self.pending_force_idr |= force_idr;
        match &mut self.backend {
            EncoderBackend::Nvenc {
                encoder,
                encode_texture,
                ..
            } => {
                if self.pending_force_idr {
                    // A real session recreate (destroy + brand-new NVENC
                    // session) rather than the live `NvEncReconfigureEncoder`
                    // call: two independent real-hardware tests of forcing a
                    // keyframe through live reconfigure (2026-07-08, both
                    // with and without also forcing resetEncoder on pure
                    // bitrate nudges) reproduced a permanent post-breakdown
                    // black screen on every platform watching a Windows host.
                    // The one thing that never broke down for the same host
                    // is a brand-new NVENC session's first frame, which is
                    // always a real IDR regardless of driver quirks around
                    // resetEncoder/FORCEIDR (see the 2026-07-05 PROGRESS
                    // entry). Recreate is visibly disruptive per event (see
                    // the 2026-07-08 entry that motivated moving away from it
                    // in the first place, and 2026-07-18's stutter regression
                    // from applying it to every bitrate nudge too) — reserved
                    // for genuine forced-IDR needs (connect, reconnect, real
                    // client recovery requests), which are comparatively
                    // rare, unlike routine bitrate adjustments below.
                    const RECREATE_COALESCE_WINDOW: std::time::Duration =
                        std::time::Duration::from_millis(1000);
                    let coalescing = self
                        .last_reconfigure
                        .map(|t| t.elapsed() < RECREATE_COALESCE_WINDOW)
                        .unwrap_or(false);
                    if !coalescing {
                        crate::logging::log("NVENC: recreating encoder session (forced IDR)");
                        match NvencEncoder::new(
                            &self.device,
                            encode_texture,
                            self.width,
                            self.height,
                            self.fps,
                            self.bitrate_bps,
                            encoder.codec(),
                            encoder.is_hdr10(),
                        ) {
                            Ok(fresh) => {
                                *encoder = fresh;
                                self.bitrate_dirty = false;
                                self.pending_force_idr = false;
                                self.last_reconfigure = Some(std::time::Instant::now());
                            }
                            Err(e) => crate::logging::log(&format!(
                                "NVENC: session recreate failed, continuing with existing session: {e}"
                            )),
                        }
                    }
                } else if self.bitrate_dirty {
                    // Pure bitrate nudges (no keyframe need) stay on the
                    // cheap live-reconfigure path with resetEncoder=0 —
                    // this specific combination (bitrate-only, no forced
                    // IDR) was never isolated as the cause of the black
                    // screen on its own; every real-hardware test that
                    // reproduced it also had forced-IDR reconfigures going
                    // through the same live-reconfigure call. Adaptive
                    // bitrate fires far too often (every 1-15s during a
                    // real session) for a full recreate to be tolerable —
                    // that's what caused 2026-07-18's "one frame every few
                    // seconds" regression.
                    crate::logging::log(&format!(
                        "NVENC: reconfiguring encoder session (bitrate -> {} kbps)",
                        self.bitrate_bps / 1000
                    ));
                    match encoder.reconfigure(self.bitrate_bps, false) {
                        Ok(()) => self.bitrate_dirty = false,
                        Err(e) => crate::logging::log(&format!(
                            "NVENC: reconfigure failed, continuing with existing session: {e}"
                        )),
                    }
                }
                encoder.encode(false)
            }
            EncoderBackend::Software { transform, nv12 } => {
                if force_idr {
                    let _ = set_codec_u32(transform, &CODECAPI_FORCEKEYFRAME, 1);
                }
                let sample = build_nv12_sample(nv12, self.fps, self.frame_index)?;
                self.frame_index += 1;
                ctx(transform.ProcessInput(0, &sample, 0), "ProcessInput")?;
                drain_output_sync(transform, self.width, self.height)
            }
        }
    }
}

unsafe fn build_nv12_sample(nv12: &[u8], fps: u32, frame_index: i64) -> Result<IMFSample, String> {
    let sample = ctx(MFCreateSample(), "MFCreateSample")?;
    let buffer = ctx(
        MFCreateMemoryBuffer(nv12.len() as u32),
        "MFCreateMemoryBuffer",
    )?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    ctx(buffer.Lock(&mut ptr, None, None), "buffer.Lock")?;
    std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
    ctx(buffer.Unlock(), "buffer.Unlock")?;
    ctx(
        buffer.SetCurrentLength(nv12.len() as u32),
        "SetCurrentLength",
    )?;
    ctx(sample.AddBuffer(&buffer), "AddBuffer")?;

    let hundred_ns = 10_000_000i64 / fps.max(1) as i64;
    ctx(
        sample.SetSampleTime(frame_index * hundred_ns),
        "SetSampleTime",
    )?;
    ctx(sample.SetSampleDuration(hundred_ns), "SetSampleDuration")?;
    Ok(sample)
}

/// Pull every access unit the encoder has ready (a sync MFT must be drained
/// to `NEED_MORE_INPUT` before the next `ProcessInput`). Returns the first
/// (in practice only) AU produced this round.
unsafe fn drain_output_sync(
    transform: &IMFTransform,
    width: u32,
    height: u32,
) -> Result<Option<EncodedFrame>, String> {
    let info = ctx(transform.GetOutputStreamInfo(0), "GetOutputStreamInfo")?;
    let mut result = None;

    loop {
        let out_sample = ctx(MFCreateSample(), "out MFCreateSample")?;
        let out_buffer = ctx(
            MFCreateMemoryBuffer(info.cbSize.max(1)),
            "out MFCreateMemoryBuffer",
        )?;
        ctx(out_sample.AddBuffer(&out_buffer), "out AddBuffer")?;

        let mut data = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(Some(out_sample)),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = transform.ProcessOutput(0, &mut data, &mut status);
        // Reclaim (and thus release) the sample we handed to the MFT.
        let produced = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut data[0].pSample,
            std::mem::ManuallyDrop::new(None),
        ));

        match r {
            Ok(()) => {
                if result.is_none() {
                    if let Some(sample) = &produced {
                        result = Some(read_access_unit(sample, width, height)?);
                    }
                }
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
            Err(e) => return Err(format!("ProcessOutput: {e}")),
        }
    }

    Ok(result)
}

unsafe fn read_access_unit(
    sample: &IMFSample,
    width: u32,
    height: u32,
) -> Result<EncodedFrame, String> {
    let media_buffer = ctx(
        sample.ConvertToContiguousBuffer(),
        "ConvertToContiguousBuffer",
    )?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0u32;
    ctx(
        media_buffer.Lock(&mut ptr, None, Some(&mut len)),
        "out Lock",
    )?;
    let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
    let _ = media_buffer.Unlock();

    // `MFSampleExtension_CleanPoint` is optional and this encoder doesn't
    // reliably set it — reading it as "keyframe" produced a false
    // negative on every single access unit (including ones that really
    // were forced IDRs), which looked exactly like force_idr silently
    // not working. Parse the actual NAL unit type instead — the same
    // ground truth the Android client already uses to decide this.
    let keyframe = streambeam_core::access_unit_is_keyframe(&bytes);

    Ok(EncodedFrame {
        data: bytes,
        keyframe,
        width,
        height,
        // The MF software path has no reference invalidation — see
        // `EncodedFrame::timestamp`.
        timestamp: 0,
    })
}

unsafe fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut level = D3D_FEATURE_LEVEL::default();
    ctx(
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut level),
            Some(&mut context),
        ),
        "D3D11CreateDevice",
    )?;
    Ok((
        device.ok_or("no D3D11 device")?,
        context.ok_or("no D3D11 context")?,
    ))
}

/// `STREAMBEAM_HDR_CAPTURE=dwm` forces the BGRA8 path on an HDR desktop, so
/// Windows does the HDR→SDR conversion instead of our shader. It exists to A/B
/// the two against identical content: as of 2026-08-17 our tone-map visibly
/// washes out an HDR desktop (blacks lifted, saturated colours pulled toward
/// grey) where DWM's looks right, and comparing them on the same screen is the
/// only way to tell a wrong constant from a wrong transfer function. DWM's own
/// conversion has the white-level flicker this shader was written to fix, so
/// it's a diagnostic, not a default.
///
/// Checked by *both* capture sources — a toggle that only bound the duplication
/// path would silently revert to our shader the moment WGC took over, which is
/// exactly when a game is on screen and you're looking at the colours.
/// `STREAMBEAM_HDR=passthrough` forces HDR10 regardless of what the client
/// asked for.
///
/// Negotiation (`VideoCaps::hdr`) is the real gate; this stays as a testing
/// override so the encoder path can be exercised with tools that can't display
/// the result — `devclient --hdr` advertises the cap properly, but forcing it
/// is useful when bisecting whether a client's advertisement is the problem.
fn hdr10_forced() -> bool {
    std::env::var("STREAMBEAM_HDR")
        .map(|v| v.eq_ignore_ascii_case("passthrough"))
        .unwrap_or(false)
}

fn dwm_tone_map_forced() -> bool {
    std::env::var("STREAMBEAM_HDR_CAPTURE")
        .map(|v| v.eq_ignore_ascii_case("dwm"))
        .unwrap_or(false)
}

/// Whether the output is presenting HDR10 (PQ / BT.2020), i.e. whether the
/// FP16 scRGB capture + in-shader tone-map path applies.
unsafe fn output_is_hdr(output1: &IDXGIOutput1) -> bool {
    output1
        .cast::<IDXGIOutput6>()
        .and_then(|o6| o6.GetDesc1())
        .map(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
        .unwrap_or(false)
}

/// Same question as [`output_is_hdr`], asked from a device rather than an
/// output — the WGC fallback has to pick its pixel format without going
/// through `create_duplication`, and picking the wrong one would feed the
/// tone-map shader a format it doesn't expect.
unsafe fn desktop_is_hdr(device: &ID3D11Device, allow_hdr: bool) -> bool {
    if !allow_hdr || dwm_tone_map_forced() {
        return false;
    }
    (|| -> Option<bool> {
        let dxgi_device: IDXGIDevice = device.cast().ok()?;
        let adapter: IDXGIAdapter = dxgi_device.GetAdapter().ok()?;
        let output1: IDXGIOutput1 = adapter.EnumOutputs(0).ok()?.cast().ok()?;
        Some(output_is_hdr(&output1))
    })()
    .unwrap_or(false)
}

unsafe fn create_duplication(
    device: &ID3D11Device,
    allow_hdr: bool,
) -> Result<(IDXGIOutputDuplication, u32, u32, f32), String> {
    let dxgi_device: IDXGIDevice = ctx(device.cast(), "device.cast IDXGIDevice")?;
    let adapter: IDXGIAdapter = ctx(dxgi_device.GetAdapter(), "GetAdapter")?;
    let output = ctx(adapter.EnumOutputs(0), "EnumOutputs(0)")?;
    let output1: IDXGIOutput1 = ctx(output.cast(), "output.cast IDXGIOutput1")?;

    if let Ok(output6) = output1.cast::<IDXGIOutput6>()
        && let Ok(desc1) = output6.GetDesc1()
    {
        crate::logging::log(&format!(
            "display color space: {:?}, {}-bit, max luminance {:.0} nits (HDR if ColorSpace is G2084_*)",
            desc1.ColorSpace, desc1.BitsPerColor, desc1.MaxLuminance
        ));
    }

    // Plain `IDXGIOutput1::DuplicateOutput` hands back frames in whatever
    // format the desktop is actually composited in — on an HDR/WCG desktop
    // (`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`, logged above) that's a
    // 16-bit-per-channel float buffer (scRGB/PQ), not the 8-bit BGRA
    // `create_staging`'s `CopyResource` target assumes. Reading that as if
    // it were 8-bit produces exactly the garbled, motion-heavy artifacts
    // reported (each row misaligned by the format's real, larger stride).
    // `IDXGIOutput5::DuplicateOutput1` lets us explicitly request an 8-bit
    // SDR format instead — Windows itself does the HDR→SDR tone-mapping
    // before handing us the frame, the same as it always did for the (until
    // now, accidentally-only-correct-in-SDR) plain `DuplicateOutput` path.
    let output5: IDXGIOutput5 = ctx(output1.cast(), "output1.cast IDXGIOutput5")?;

    // On an HDR desktop, requesting BGRA8 makes *DWM* do the HDR->SDR
    // conversion per frame — and its tone mapper intermittently composites
    // single frames at a different white level (measured live 2026-07-19:
    // identical static content flipping YAVG 93.4 -> 44.1 -> 93.4 on
    // adjacent frames), the brightness-flashing artifact seen on every
    // client and codec since the project started. Capture the native FP16
    // scRGB image instead and tone-map with fixed parameters in our own
    // shader (`GpuScaler`'s `ps_tonemap`), like Sunshine. Only the NVENC
    // path has that shader stage (`allow_hdr`); the software-encoder
    // fallback keeps BGRA8/DWM conversion, flicker and all.
    let hdr = allow_hdr && !dwm_tone_map_forced() && output_is_hdr(&output1);
    if dwm_tone_map_forced() && output_is_hdr(&output1) {
        crate::logging::log(
            "STREAMBEAM_HDR_CAPTURE=dwm: capturing BGRA8 and letting Windows tone-map",
        );
    }
    let format = if hdr {
        DXGI_FORMAT_R16G16B16A16_FLOAT
    } else {
        DXGI_FORMAT_B8G8R8A8_UNORM
    };
    let sdr_white_nits = if hdr {
        let nits = query_sdr_white_nits(&output.GetDesc().map(|d| d.DeviceName).unwrap_or_default())
            .unwrap_or(DEFAULT_SDR_WHITE_NITS);
        crate::logging::log(&format!(
            "HDR desktop: capturing FP16 scRGB, tone-mapping in-shader at SDR white {nits:.0} nits"
        ));
        nits
    } else {
        DEFAULT_SDR_WHITE_NITS
    };
    let duplication = ctx(
        output5.DuplicateOutput1(device, 0, &[format]),
        "DuplicateOutput1",
    )?;
    let desc = duplication.GetDesc();

    Ok((
        duplication,
        desc.ModeDesc.Width,
        desc.ModeDesc.Height,
        sdr_white_nits,
    ))
}

/// Windows' default when the "SDR content brightness" slider can't be read
/// — the slider's own default position maps SDR white to 200 nits.
const DEFAULT_SDR_WHITE_NITS: f32 = 200.0;

/// The user's "SDR content brightness" setting for the output with this GDI
/// device name (e.g. `\\.\DISPLAY1`), in nits — the white level Windows
/// itself renders SDR content at on an HDR desktop, which our HDR->SDR
/// tone-map must match for the stream not to look darker/brighter than the
/// local screen. `DISPLAYCONFIG_SDR_WHITE_LEVEL.SDRWhiteLevel` is in units
/// of 1/1000, where 1000 = 80 nits.
unsafe fn query_sdr_white_nits(device_name: &[u16; 32]) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };

    let mut num_paths = 0u32;
    let mut num_modes = 0u32;
    if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes)
        != windows::Win32::Foundation::WIN32_ERROR(0)
    {
        return None;
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
    if QueryDisplayConfig(
        QDC_ONLY_ACTIVE_PATHS,
        &mut num_paths,
        paths.as_mut_ptr(),
        &mut num_modes,
        modes.as_mut_ptr(),
        None,
    ) != windows::Win32::Foundation::WIN32_ERROR(0)
    {
        return None;
    }
    for path in &paths[..num_paths as usize] {
        // Match on the *source* GDI name (that's what DXGI_OUTPUT_DESC
        // carries); the SDR white level itself hangs off the *target*.
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
            continue;
        }
        if source.viewGdiDeviceName != *device_name {
            continue;
        }
        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
        white.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
        white.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
        white.header.adapterId = path.targetInfo.adapterId;
        white.header.id = path.targetInfo.id;
        if DisplayConfigGetDeviceInfo(&mut white.header) == 0 && white.SDRWhiteLevel > 0 {
            return Some(white.SDRWhiteLevel as f32 * 80.0 / 1000.0);
        }
    }
    None
}

unsafe fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: D3D11_BIND_FLAG(0).0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    ctx(
        device.CreateTexture2D(&desc, None, Some(&mut texture)),
        "CreateTexture2D staging",
    )?;
    texture.ok_or_else(|| "no staging texture".into())
}

/// GPU-resident texture registered with NVENC (`D3D11_USAGE_DEFAULT` +
/// `D3D11_BIND_RENDER_TARGET`, required for `NvEncRegisterResource` to accept
/// it as a DirectX input resource) — the captured frame is `CopyResource`'d
/// into this every tick; NVENC reads it directly, no CPU round trip.
/// `hdr10` selects the 10-bit render target NVENC's `ABGR10` input needs; the
/// SDR path stays 8-bit BGRA.
unsafe fn create_encode_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    hdr10: bool,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: if hdr10 {
            DXGI_FORMAT_R10G10B10A2_UNORM
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    ctx(
        device.CreateTexture2D(&desc, None, Some(&mut texture)),
        "CreateTexture2D encode",
    )?;
    texture.ok_or_else(|| "no encode texture".into())
}

/// Create + configure the built-in Media Foundation software H.264 encoder —
/// used only when NVENC isn't available (no NVIDIA GPU, or it failed to init).
unsafe fn create_software_encoder(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFTransform, String> {
    let transform: IMFTransform = ctx(
        CoCreateInstance(&CLSID_CMSH264ENCODER, None, CLSCTX_INPROC_SERVER),
        "CoCreateInstance H264 encoder",
    )?;

    let bitrate = ((width * height) as u64 * fps as u64 / 15).clamp(2_000_000, 20_000_000) as u32;

    // ICodecAPI properties (bitrate/rate-control/GOP/low-latency) must be set
    // before SetOutputType/SetInputType — this encoder bakes them into the
    // type negotiation, and setting them afterward is silently accepted (no
    // error) but has no effect, which is exactly what made an earlier GOP fix
    // look like a no-op. Best effort: log rather than silently ignore if a
    // property really is rejected.
    let gop = fps.max(1) * 3;
    for (label, result) in [
        (
            "AVLOWLATENCYMODE",
            set_codec_bool(&transform, &CODECAPI_AVLOWLATENCYMODE, true),
        ),
        (
            "RATECONTROL=CBR",
            set_codec_u32(&transform, &CODECAPI_RATECONTROL, 0),
        ),
        (
            "MEANBITRATE",
            set_codec_u32(&transform, &CODECAPI_MEANBITRATE, bitrate),
        ),
        // We tried relying entirely on on-demand IDR (`CODECAPI_
        // AVEncVideoForceKeyFrame`, set in `encode()`) with a very long GOP
        // here, matching how Sunshine/Moonlight avoid periodic keyframe
        // bandwidth bursts — but ground-truth NAL-type inspection of the
        // actual output showed the Microsoft software H.264 MFT never
        // delivers a forced IDR: dozens of on-demand requests, zero resulting
        // keyframes in the bitstream. Sunshine's own source documents this
        // exact limitation ("Fixed GOP for encoders that don't support
        // on-demand IDR, e.g. Media Foundation"). So: a moderate periodic GOP
        // it is — frequent enough that a client that fell behind actually
        // gets a recovery point, infrequent enough (a keyframe every few
        // seconds, not every second) that the bandwidth burst stays modest.
        ("GOPSIZE", set_codec_u32(&transform, &CODECAPI_GOPSIZE, gop)),
    ] {
        if let Err(e) = result {
            crate::logging::log(&format!(
                "encoder codec property {label} rejected by driver: {e}"
            ));
        }
    }

    // Output first (required by MFTs), then the matching NV12 input.
    let out_type = ctx(MFCreateMediaType(), "MFCreateMediaType out")?;
    ctx(
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video),
        "out major",
    )?;
    ctx(
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264),
        "out subtype",
    )?;
    ctx(
        out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate),
        "out bitrate",
    )?;
    ctx(
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
        "out interlace",
    )?;
    ctx(
        out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32),
        "out profile",
    )?;
    ctx(
        set_size(&out_type, &MF_MT_FRAME_SIZE, width, height),
        "out size",
    )?;
    ctx(set_ratio(&out_type, &MF_MT_FRAME_RATE, fps, 1), "out rate")?;
    ctx(
        set_ratio(&out_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1),
        "out par",
    )?;
    ctx(transform.SetOutputType(0, &out_type, 0), "SetOutputType")?;

    let in_type = ctx(MFCreateMediaType(), "MFCreateMediaType in")?;
    ctx(
        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video),
        "in major",
    )?;
    ctx(
        in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12),
        "in subtype",
    )?;
    ctx(
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
        "in interlace",
    )?;
    ctx(
        set_size(&in_type, &MF_MT_FRAME_SIZE, width, height),
        "in size",
    )?;
    ctx(set_ratio(&in_type, &MF_MT_FRAME_RATE, fps, 1), "in rate")?;
    ctx(
        set_ratio(&in_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1),
        "in par",
    )?;
    ctx(transform.SetInputType(0, &in_type, 0), "SetInputType")?;

    ctx(
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0),
        "NOTIFY_BEGIN_STREAMING",
    )?;
    ctx(
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0),
        "NOTIFY_START_OF_STREAM",
    )?;

    Ok(transform)
}

unsafe fn set_size(t: &IMFMediaType, key: &GUID, w: u32, h: u32) -> windows::core::Result<()> {
    t.SetUINT64(key, (u64::from(w) << 32) | u64::from(h))
}

unsafe fn set_ratio(t: &IMFMediaType, key: &GUID, num: u32, den: u32) -> windows::core::Result<()> {
    t.SetUINT64(key, (u64::from(num) << 32) | u64::from(den))
}

unsafe fn set_codec_u32(transform: &IMFTransform, key: &GUID, value: u32) -> Result<(), String> {
    let codec: ICodecAPI = ctx(transform.cast(), "cast ICodecAPI")?;
    let var = VARIANT::from(value);
    ctx(codec.SetValue(key, &var), "ICodecAPI.SetValue u32")
}

unsafe fn set_codec_bool(transform: &IMFTransform, key: &GUID, value: bool) -> Result<(), String> {
    let codec: ICodecAPI = ctx(transform.cast(), "cast ICodecAPI")?;
    let var = VARIANT::from(value);
    ctx(codec.SetValue(key, &var), "ICodecAPI.SetValue bool")
}

/// BGRA (from Desktop Duplication) → NV12 (what the software encoder wants),
/// BT.601 limited. Convert (and, when `dst` is smaller than `src`,
/// nearest-neighbor downscale) a captured BGRA desktop frame to NV12.
#[allow(clippy::too_many_arguments)]
fn bgra_to_nv12(
    src: &[u8],
    stride: usize,
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    out: &mut [u8],
) {
    let (sw, sh) = (src_width as usize, src_height as usize);
    let (w, h) = (dst_width as usize, dst_height as usize);
    let y_plane = w * h;

    // Nearest-neighbor source pixel for destination (x, y); degenerates to an
    // identity mapping when src and dst are the same size.
    let pixel_at = |x: usize, y: usize| -> (i32, i32, i32) {
        let sx = (x * sw / w).min(sw.saturating_sub(1));
        let sy = (y * sh / h).min(sh.saturating_sub(1));
        let p = sy * stride + sx * 4;
        (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32)
    };

    for y in 0..h {
        for x in 0..w {
            let (b, g, r) = pixel_at(x, y);
            let yy = (66 * r + 129 * g + 25 * b + 128 >> 8) + 16;
            out[y * w + x] = yy.clamp(0, 255) as u8;
        }
    }

    // Interleaved UV plane at half resolution (sample top-left of each 2×2 block).
    let mut uv = y_plane;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (b, g, r) = pixel_at(x, y);
            let u = (-38 * r - 74 * g + 112 * b + 128 >> 8) + 128;
            let v = (112 * r - 94 * g - 18 * b + 128 >> 8) + 128;
            out[uv] = u.clamp(0, 255) as u8;
            out[uv + 1] = v.clamp(0, 255) as u8;
            uv += 2;
        }
    }
}

/// Scale `(w, h)` down so the longer side is at most `max_dim`, preserving
/// aspect ratio and keeping both dimensions even (required by NV12/H.264).
/// A no-op when `(w, h)` already fits.
fn scaled_dimensions(w: u32, h: u32, max_dim: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max_dim {
        return (w & !1, h & !1);
    }
    let scale = f64::from(max_dim) / f64::from(longest);
    let sw = ((f64::from(w) * scale) as u32).max(2) & !1;
    let sh = ((f64::from(h) * scale) as u32).max(2) & !1;
    (sw, sh)
}
