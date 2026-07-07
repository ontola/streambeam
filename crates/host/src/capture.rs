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
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
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

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder, default_bitrate_bps};
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
    _thread: JoinHandle<()>,
}

impl DesktopEncoder {
    /// `max_encode_dim` caps the encoded video's longest side (native capture
    /// resolution is unaffected) — see the field doc on `Pipeline` for why.
    pub fn new(fps: u32, max_encode_dim: u32) -> Result<Self, String> {
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32), String>>();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<Option<EncodedFrame>>(4);
        let force_idr = Arc::new(AtomicBool::new(false));
        let thread_force = force_idr.clone();
        let pending_bitrate = Arc::new(Mutex::new(None));
        let thread_bitrate = pending_bitrate.clone();
        let codec_config = Arc::new(Mutex::new(None));
        let thread_codec_config = codec_config.clone();

        let thread = std::thread::Builder::new()
            .name("streambeam-capture".into())
            .spawn(move || {
                run_pipeline(
                    fps,
                    max_encode_dim,
                    init_tx,
                    req_rx,
                    frame_tx,
                    thread_force,
                    thread_bitrate,
                    thread_codec_config,
                )
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match init_rx.recv() {
            Ok(Ok((width, height))) => Ok(Self {
                width,
                height,
                codec_config,
                req_tx,
                frame_rx,
                force_idr,
                pending_bitrate,
                _thread: thread,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("capture thread exited during init".into()),
        }
    }
}

// TODO(M2): real NVENC/AMF/QSV NvEncInvalidateRefFrames-equivalent support.
// This impl currently relies on VideoEncoder::invalidate_ref_frames's default
// (falls back to force_idr — always recovers, just not cheaply). Wiring real
// hardware invalidation here will need a map from StreamBeam's wrapping
// `frame_id` (assigned per-frame in `server::stream_video`, after encoding)
// to whatever reference-frame handle/index the chosen hardware encoder uses
// internally (NVENC reference picture indices, etc.) — that mapping doesn't
// exist yet and isn't built by this change.
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

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.lock().unwrap().clone()
    }
}

/// Body of the capture thread: init, report dims, then serve encode requests.
fn run_pipeline(
    fps: u32,
    max_encode_dim: u32,
    init_tx: Sender<Result<(u32, u32), String>>,
    req_rx: Receiver<Request>,
    frame_tx: SyncSender<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
    pending_bitrate: Arc<Mutex<Option<u32>>>,
    codec_config: Arc<Mutex<Option<Vec<u8>>>>,
) {
    let mut pipe = match Pipeline::new(fps, max_encode_dim) {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    if init_tx.send(Ok((pipe.width, pipe.height))).is_err() {
        return;
    }

    while let Ok(req) = req_rx.recv() {
        match req {
            Request::ForceIdr | Request::SetBitrate => {
                // Both flags/values are also read in the Next arm below;
                // nothing to do here beyond having woken up a thread that
                // might otherwise be parked waiting on the next real tick.
            }
            Request::Next => {
                let want_idr = force_idr.swap(false, Ordering::Relaxed);
                if let Some(bps) = pending_bitrate.lock().unwrap().take() {
                    pipe.set_bitrate(bps);
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
    /// by recreating the NVENC session, the same mechanism already used to
    /// force a real IDR (see the long comment in `encode()`), since NVENC
    /// has no cheaper live-reconfigure path wired up here yet.
    bitrate_bps: u32,
    /// Set by `set_bitrate` when `bitrate_bps` changed since the last NVENC
    /// session recreate; cleared once a recreate has actually applied it.
    bitrate_dirty: bool,
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
    /// Rolling per-report-window totals (reset every 90 calls alongside the
    /// counters above) for pinpointing where per-frame time actually goes.
    stat_capture_us: u64,
    stat_encode_us: u64,
}

fn ctx<T>(r: windows::core::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

impl Pipeline {
    /// `max_encode_dim` caps the longest encoded side. The FEC layer addresses
    /// shards with a `u16` count (see `MAX_DATA_SHARDS` in `streambeam_proto`),
    /// and a very large keyframe (native 4K, say) can still exceed it — a
    /// phone screen has no use for that much detail anyway.
    fn new(fps: u32, max_encode_dim: u32) -> Result<Self, String> {
        unsafe {
            // MTA so the objects can be used from this thread freely.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            ctx(MFStartup(MF_VERSION, MFSTARTUP_FULL), "MFStartup")?;

            let (device, context) = create_d3d_device()?;
            let (duplication, capture_width, capture_height) = create_duplication(&device)?;
            let staging = create_staging(&device, capture_width, capture_height)?;
            let (width, height) = scaled_dimensions(capture_width, capture_height, max_encode_dim);
            if (width, height) != (capture_width, capture_height) {
                crate::logging::log(&format!(
                    "downscaling capture {capture_width}x{capture_height} -> {width}x{height} to stay within FEC shard limits"
                ));
            }

            let bitrate_bps = default_bitrate_bps(width, height, fps);
            let nvenc_attempt: Result<(NvencEncoder, ID3D11Texture2D, Option<GpuScaler>), String> =
                (|| {
                    let tex = create_encode_texture(&device, width, height)?;
                    let encoder =
                        NvencEncoder::new(&device, &tex, width, height, fps, bitrate_bps)?;
                    let scaler = if (capture_width, capture_height) != (width, height) {
                        Some(GpuScaler::new(&device, &context, width, height, &tex)?)
                    } else {
                        None
                    };
                    Ok((encoder, tex, scaler))
                })();
            let backend = match nvenc_attempt {
                Ok((encoder, encode_texture, scaler)) => {
                    crate::logging::log("using direct NVENC hardware encoder");
                    EncoderBackend::Nvenc {
                        encoder,
                        encode_texture,
                        scaler,
                    }
                }
                Err(e) => {
                    crate::logging::log(&format!(
                        "NVENC unavailable ({e}) — falling back to Media Foundation software encoder"
                    ));
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
                codec_config,
                have_last: false,
                frame_index: 0,
                stat_calls: 0,
                stat_acquired: 0,
                stat_timeout: 0,
                stat_access_lost: 0,
                consecutive_access_lost: 0,
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

        // Neither backend reliably exposes SPS/PPS ahead of the first real
        // frame (the software MFT's `MF_MT_MPEG_SEQUENCE_HEADER` requires a
        // frame to have gone through; NVENC's preset config doesn't carry it
        // either) — pull SPS/PPS straight out of the first keyframe's
        // Annex-B bytes instead, which works regardless of encoder vendor.
        if self.codec_config.is_none() {
            if let Ok(Some(frame)) = &result {
                if frame.keyframe {
                    if let Some((sps, pps)) =
                        streambeam_core::extract_avc_decoder_config(&frame.data)
                    {
                        let mut config = streambeam_core::nal_with_start_code(&sps);
                        config.extend(streambeam_core::nal_with_start_code(&pps));
                        self.codec_config = Some(config);
                    }
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
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match self
            .duplication
            .AcquireNextFrame(100, &mut info, &mut resource)
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
                // First few: log and retry immediately, matching the pacing
                // of a real transient loss (display mode change) which
                // typically clears within one or two frames.
                if self.consecutive_access_lost <= 3 {
                    crate::logging::log(&format!("duplication access lost, recreating: {e}"));
                } else if self.consecutive_access_lost == 4 {
                    // Recreating hasn't helped 3 times in a row — this isn't
                    // a transient loss anymore. The two conditions that look
                    // exactly like this and that recreating can't fix: the
                    // display is powered off (idle timeout) or the session
                    // is locked. Say so once, then back off instead of
                    // spamming host.log every ~33ms indefinitely.
                    crate::logging::log(&format!(
                        "duplication access lost, {} consecutive failures — Desktop \
                         Duplication doesn't work while the display is off or the \
                         session is locked; will keep retrying quietly every 2s \
                         until it recovers ({e})",
                        self.consecutive_access_lost
                    ));
                } else if self.consecutive_access_lost % 60 == 0 {
                    crate::logging::log(&format!(
                        "duplication access lost, still failing after {} attempts — \
                         check the display is on and the session unlocked",
                        self.consecutive_access_lost
                    ));
                }
                match create_duplication(&self.device) {
                    Ok((dup, _, _)) => self.duplication = dup,
                    // Recreation itself can fail outright while the display
                    // is off (no output to enumerate) — that's the same
                    // condition, not a new error; keep backing off instead
                    // of propagating it as a hard stream error.
                    Err(_) => {}
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

            match &self.backend {
                EncoderBackend::Nvenc {
                    encode_texture,
                    scaler,
                    ..
                } => match scaler {
                    Some(scaler) => scaler.scale(&texture)?,
                    None => self.context.CopyResource(encode_texture, &texture),
                },
                EncoderBackend::Software { .. } => {
                    self.context.CopyResource(&self.staging, &texture);
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
        })();

        let _ = self.duplication.ReleaseFrame();
        result
    }

    /// Queue a new target bitrate — applied on the next `encode()` call via
    /// the same session-recreate mechanism `force_idr` uses (see its comment
    /// in `encode()`). Called from `server.rs`'s adaptive bitrate controller
    /// and/or a client's explicit `SetBitrate` request.
    fn set_bitrate(&mut self, bps: u32) {
        if bps != self.bitrate_bps {
            self.bitrate_bps = bps;
            self.bitrate_dirty = true;
        }
    }

    unsafe fn encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        match &mut self.backend {
            EncoderBackend::Nvenc {
                encoder,
                encode_texture,
                ..
            } => {
                if force_idr || self.bitrate_dirty {
                    // NVENC's per-picture NV_ENC_PIC_FLAG_FORCEIDR flag is
                    // set correctly (see nvenc.rs) but empirically the
                    // driver still doesn't produce a real IDR NAL from it —
                    // almost certainly the UltraLowLatency preset's
                    // codec-specific `idrPeriod` (a field inside the opaque
                    // per-codec config union we deliberately never touch)
                    // overriding it to "never auto-insert", and that
                    // override apparently also swallows explicit force
                    // requests on this driver. Rather than reach into that
                    // union (real risk of another layout bug), use the one
                    // IDR mechanism already proven 100% reliable: a brand
                    // new NVENC session's first frame is always a real IDR.
                    // Also the only bitrate-reconfigure path implemented so
                    // far — no live `NvEncReconfigureEncoder` call, just a
                    // fresh session built with the new target.
                    let mut reasons = Vec::new();
                    if force_idr {
                        reasons.push("forced IDR".to_string());
                    }
                    if self.bitrate_dirty {
                        reasons.push(format!("bitrate -> {} kbps", self.bitrate_bps / 1000));
                    }
                    crate::logging::log(&format!(
                        "NVENC: recreating encoder session ({})",
                        reasons.join(", ")
                    ));
                    match NvencEncoder::new(
                        &self.device,
                        encode_texture,
                        self.width,
                        self.height,
                        self.fps,
                        self.bitrate_bps,
                    ) {
                        Ok(fresh) => {
                            *encoder = fresh;
                            self.bitrate_dirty = false;
                        }
                        Err(e) => crate::logging::log(&format!(
                            "NVENC: session recreate failed, continuing with existing session: {e}"
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

unsafe fn create_duplication(
    device: &ID3D11Device,
) -> Result<(IDXGIOutputDuplication, u32, u32), String> {
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
    let duplication = ctx(
        output5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]),
        "DuplicateOutput1",
    )?;
    let desc = duplication.GetDesc();

    Ok((duplication, desc.ModeDesc.Width, desc.ModeDesc.Height))
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
unsafe fn create_encode_texture(
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
