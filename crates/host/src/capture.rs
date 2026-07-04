// This module is Win32/COM FFI glue; each `unsafe fn` body is deliberately one
// big unsafe region rather than nesting `unsafe {}` around every call.
#![allow(unsafe_op_in_unsafe_fn)]

//! Real desktop capture (DXGI Desktop Duplication) encoded to H.264 with the
//! built-in Media Foundation encoder.
//!
//! All COM lives on one dedicated thread (COM objects have thread affinity and
//! tokio moves tasks between workers). [`DesktopEncoder`] talks to that thread
//! over channels and implements [`VideoEncoder`], so `server.rs` can swap it in
//! for the mock without caring how frames are produced.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_FLAG, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_FLAG, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VARIANT;

use crate::encode::{EncodeError, EncodedFrame, VideoEncoder};

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
}

/// Video encoder backed by a background capture+encode thread.
pub struct DesktopEncoder {
    width: u32,
    height: u32,
    codec_config: Option<Vec<u8>>,
    req_tx: Sender<Request>,
    frame_rx: Receiver<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl DesktopEncoder {
    /// `max_encode_dim` caps the encoded video's longest side (native capture
    /// resolution is unaffected) — see the field doc on `Pipeline` for why.
    pub fn new(fps: u32, max_encode_dim: u32) -> Result<Self, String> {
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32, Option<Vec<u8>>), String>>();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<Option<EncodedFrame>>(4);
        let force_idr = Arc::new(AtomicBool::new(false));
        let thread_force = force_idr.clone();

        let thread = std::thread::Builder::new()
            .name("streambeam-capture".into())
            .spawn(move || run_pipeline(fps, max_encode_dim, init_tx, req_rx, frame_tx, thread_force))
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match init_rx.recv() {
            Ok(Ok((width, height, codec_config))) => Ok(Self {
                width,
                height,
                codec_config,
                req_tx,
                frame_rx,
                force_idr,
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

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.clone()
    }
}

/// Body of the capture thread: init, report dims, then serve encode requests.
fn run_pipeline(
    fps: u32,
    max_encode_dim: u32,
    init_tx: Sender<Result<(u32, u32, Option<Vec<u8>>), String>>,
    req_rx: Receiver<Request>,
    frame_tx: SyncSender<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
) {
    let mut pipe = match Pipeline::new(fps, max_encode_dim) {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    if init_tx
        .send(Ok((pipe.width, pipe.height, pipe.codec_config.clone())))
        .is_err()
    {
        return;
    }

    while let Ok(req) = req_rx.recv() {
        match req {
            Request::ForceIdr => {
                // The flag is also read in encode(); nothing else to do here.
            }
            Request::Next => {
                let want_idr = force_idr.swap(false, Ordering::Relaxed);
                let frame = pipe.capture_and_encode(want_idr).unwrap_or_else(|e| {
                    crate::logging::log(&format!("capture/encode error: {e}"));
                    None
                });
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
        }
    }
}

struct Pipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    transform: IMFTransform,
    /// Whether `transform` is a hardware encoder MFT, which Windows requires
    /// to run under the async transform contract (event-driven
    /// `ProcessInput`/`ProcessOutput` — see `encode_async`/`drain_output_async`)
    /// rather than the eager synchronous calls the software encoder accepts.
    is_async: bool,
    /// Only used when `is_async`: the encoder's event source, and whether
    /// we're currently allowed to `ProcessInput` (set by a `METransformNeedInput`
    /// event, cleared once we've fed a sample).
    event_gen: Option<IMFMediaEventGenerator>,
    can_feed_input: bool,
    /// Native desktop resolution — the size of `staging` and of the BGRA data
    /// `capture_into_nv12` reads.
    capture_width: u32,
    capture_height: u32,
    /// Encoded resolution — `capture_{width,height}` downscaled to
    /// `max_encode_dim`, preserving aspect ratio. This is what `nv12`, the MF
    /// encoder, and `EncodedFrame` all use.
    width: u32,
    height: u32,
    fps: u32,
    codec_config: Option<Vec<u8>>,
    nv12: Vec<u8>,
    have_last: bool,
    frame_index: i64,
    // Diagnostics: logged periodically so a black/frozen stream is debuggable
    // from host.log even in a release build with no console attached.
    stat_calls: u32,
    stat_acquired: u32,
    stat_timeout: u32,
    stat_access_lost: u32,
    stat_produced: u32,
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
            let (width, height) =
                scaled_dimensions(capture_width, capture_height, max_encode_dim);
            if (width, height) != (capture_width, capture_height) {
                crate::logging::log(&format!(
                    "downscaling capture {capture_width}x{capture_height} -> {width}x{height} to stay within FEC shard limits"
                ));
            }
            let (transform, is_async, codec_config) = create_encoder(&device, width, height, fps)?;
            let event_gen: Option<IMFMediaEventGenerator> = if is_async {
                match transform.cast() {
                    Ok(evt_gen) => Some(evt_gen),
                    Err(e) => {
                        crate::logging::log(&format!(
                            "hardware encoder has no IMFMediaEventGenerator ({e}) — falling back to synchronous calls, which may fail on a true hardware MFT"
                        ));
                        None
                    }
                }
            } else {
                None
            };

            Ok(Self {
                device,
                context,
                duplication,
                staging,
                transform,
                is_async: is_async && event_gen.is_some(),
                event_gen,
                can_feed_input: false,
                capture_width,
                capture_height,
                width,
                height,
                fps,
                codec_config,
                nv12: vec![0u8; (width * height * 3 / 2) as usize],
                have_last: false,
                frame_index: 0,
                stat_calls: 0,
                stat_acquired: 0,
                stat_timeout: 0,
                stat_access_lost: 0,
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
        let captured = unsafe { self.capture_into_nv12() };
        self.stat_capture_us += t_capture.elapsed().as_micros() as u64;

        let result = captured.and_then(|()| {
            let t_encode = std::time::Instant::now();
            let r = unsafe { self.encode(force_idr) };
            self.stat_encode_us += t_encode.elapsed().as_micros() as u64;
            r
        });

        if let Ok(Some(_)) = result {
            self.stat_produced += 1;
        }
        // Roughly every 3s at 30fps — enough to catch a stuck pipeline quickly
        // without flooding host.log.
        const REPORT_WINDOW: u64 = 90;
        if self.stat_calls % REPORT_WINDOW as u32 == 0 {
            let avg_luma = average_luma(&self.nv12, self.width, self.height);
            crate::logging::log(&format!(
                "capture stats: {} calls — {} acquired, {} timeout, {} access-lost, {} AUs produced, avg Y={avg_luma}, avg capture={} ms, avg encode={} ms",
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

    unsafe fn capture_into_nv12(&mut self) -> Result<(), String> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match self
            .duplication
            .AcquireNextFrame(100, &mut info, &mut resource)
        {
            Ok(()) => {
                self.stat_acquired += 1;
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                self.stat_timeout += 1;
                // No change since last frame; reuse the last NV12 (or a black
                // frame if we have never captured yet).
                if !self.have_last {
                    self.nv12.iter_mut().enumerate().for_each(|(i, b)| {
                        *b = if i < (self.width * self.height) as usize { 16 } else { 128 };
                    });
                    self.have_last = true;
                }
                return Ok(());
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.stat_access_lost += 1;
                crate::logging::log(&format!("duplication access lost, recreating: {e}"));
                let (dup, _, _) = create_duplication(&self.device)?;
                self.duplication = dup;
                return Ok(());
            }
            Err(e) => return Err(format!("AcquireNextFrame: {e}")),
        }

        let result = (|| -> Result<(), String> {
            let resource = resource.ok_or("duplication returned no resource")?;
            let texture: ID3D11Texture2D = ctx(resource.cast(), "resource.cast texture")?;
            self.context.CopyResource(&self.staging, &texture);

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
                &mut self.nv12,
            );
            self.context.Unmap(&self.staging, 0);
            self.have_last = true;
            Ok(())
        })();

        let _ = self.duplication.ReleaseFrame();
        result
    }

    unsafe fn encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        if force_idr {
            let _ = set_codec_u32(&self.transform, &CODECAPI_FORCEKEYFRAME, 1);
        }

        let sample = ctx(MFCreateSample(), "MFCreateSample")?;
        let buffer = ctx(MFCreateMemoryBuffer(self.nv12.len() as u32), "MFCreateMemoryBuffer")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        ctx(buffer.Lock(&mut ptr, None, None), "buffer.Lock")?;
        std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), ptr, self.nv12.len());
        ctx(buffer.Unlock(), "buffer.Unlock")?;
        ctx(buffer.SetCurrentLength(self.nv12.len() as u32), "SetCurrentLength")?;
        ctx(sample.AddBuffer(&buffer), "AddBuffer")?;

        let hundred_ns = 10_000_000i64 / self.fps.max(1) as i64;
        ctx(sample.SetSampleTime(self.frame_index * hundred_ns), "SetSampleTime")?;
        ctx(sample.SetSampleDuration(hundred_ns), "SetSampleDuration")?;
        self.frame_index += 1;

        if self.is_async {
            self.encode_async(sample)
        } else {
            ctx(self.transform.ProcessInput(0, &sample, 0), "ProcessInput")?;
            self.drain_output()
        }
    }

    /// Async-MFT variant of `encode` (hardware encoders): only `ProcessInput`
    /// after a `METransformNeedInput` event, and only `ProcessOutput` after a
    /// `METransformHaveOutput` event — the eager retry-until-NEED_MORE_INPUT
    /// pattern `drain_output` uses is specific to the synchronous contract
    /// and returns `MF_E_TRANSFORM_ASYNC` on a real hardware MFT. We block
    /// only long enough to feed this round's sample (the MFT signals
    /// readiness promptly after consuming the previous one); we do NOT block
    /// waiting for this sample's output, since a pipelined hardware encoder
    /// may not emit 1:1 with input — any output already available is
    /// collected non-blockingly before returning.
    unsafe fn encode_async(&mut self, sample: IMFSample) -> Result<Option<EncodedFrame>, String> {
        let event_gen = self
            .event_gen
            .clone()
            .ok_or("async encoder has no event generator")?;
        let mut result = None;
        // IMFMediaEvent::GetType returns a raw u32, not the MF_EVENT_TYPE
        // newtype the METransform* constants are typed as.
        let need_input = METransformNeedInput.0 as u32;
        let have_output = METransformHaveOutput.0 as u32;

        // `GetEvent(MF_EVENT_FLAG_NONE)` blocks with NO timeout — if the MFT
        // ever stops signaling (driver quirk, wrong assumption about this
        // vendor's event cadence, etc.) that hangs the capture thread
        // forever with no way to recover. Poll with MF_EVENT_FLAG_NO_WAIT
        // under an explicit deadline instead, so a stuck encoder becomes a
        // logged, recoverable error (run_pipeline already treats an Err from
        // capture_and_encode as "skip this frame, keep going") rather than a
        // silent freeze.
        const EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
        let deadline = std::time::Instant::now() + EVENT_TIMEOUT;

        while !self.can_feed_input {
            match event_gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(event) => {
                    let met = ctx(event.GetType(), "event GetType")?;
                    if met == need_input {
                        self.can_feed_input = true;
                    } else if met == have_output {
                        if let Some(frame) = self.pull_output_async()? {
                            result = Some(frame);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "hardware encoder produced no METransformNeedInput event within {EVENT_TIMEOUT:?}"
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_micros(500));
                }
                Err(e) => return Err(format!("GetEvent: {e}")),
            }
        }
        ctx(self.transform.ProcessInput(0, &sample, 0), "ProcessInput (async)")?;
        self.can_feed_input = false;

        loop {
            match event_gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(event) => {
                    let met = ctx(event.GetType(), "event GetType")?;
                    if met == need_input {
                        self.can_feed_input = true;
                    } else if met == have_output {
                        if let Some(frame) = self.pull_output_async()? {
                            result = Some(frame);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(e) => return Err(format!("GetEvent: {e}")),
            }
        }

        Ok(result)
    }

    /// One `ProcessOutput` call in response to a single `METransformHaveOutput`
    /// event (async contract — each event corresponds to exactly one
    /// available output, unlike the sync retry loop in `drain_output`).
    unsafe fn pull_output_async(&mut self) -> Result<Option<EncodedFrame>, String> {
        let info = ctx(self.transform.GetOutputStreamInfo(0), "GetOutputStreamInfo")?;
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
        let r = self.transform.ProcessOutput(0, &mut data, &mut status);
        let produced = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut data[0].pSample,
            std::mem::ManuallyDrop::new(None),
        ));

        match r {
            Ok(()) => match &produced {
                Some(sample) => Ok(Some(self.read_access_unit(sample)?)),
                None => Ok(None),
            },
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
            Err(e) => Err(format!("ProcessOutput (async): {e}")),
        }
    }

    /// Pull every access unit the encoder has ready (a sync MFT must be drained
    /// to `NEED_MORE_INPUT` before the next `ProcessInput`). Returns the first
    /// (in practice only) AU produced this round.
    unsafe fn drain_output(&mut self) -> Result<Option<EncodedFrame>, String> {
        let info = ctx(self.transform.GetOutputStreamInfo(0), "GetOutputStreamInfo")?;
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
            let r = self.transform.ProcessOutput(0, &mut data, &mut status);
            // Reclaim (and thus release) the sample we handed to the MFT.
            let produced =
                std::mem::ManuallyDrop::into_inner(std::mem::replace(&mut data[0].pSample, std::mem::ManuallyDrop::new(None)));

            match r {
                Ok(()) => {
                    if result.is_none() {
                        if let Some(sample) = &produced {
                            result = Some(self.read_access_unit(sample)?);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) => return Err(format!("ProcessOutput: {e}")),
            }
        }

        Ok(result)
    }

    unsafe fn read_access_unit(&self, sample: &IMFSample) -> Result<EncodedFrame, String> {
        let media_buffer = ctx(sample.ConvertToContiguousBuffer(), "ConvertToContiguousBuffer")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        ctx(media_buffer.Lock(&mut ptr, None, Some(&mut len)), "out Lock")?;
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
            width: self.width,
            height: self.height,
        })
    }
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
    let duplication = ctx(output1.DuplicateOutput(device), "DuplicateOutput")?;
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

/// Enumerate hardware H.264 encoder MFTs (NVENC/QuickSync/AMF register one
/// via their driver) and activate the first match. Returns `None` if the
/// system has none (e.g. software-only VM), in which case `create_encoder`
/// falls back to the built-in software encoder.
///
/// Windows requires hardware MFTs to implement the *async* transform
/// contract (event-driven `ProcessInput`/`ProcessOutput`, unlike the
/// synchronous software encoder this replaces as the default) — see
/// `Pipeline::encode_async` / `drain_output_async`.
unsafe fn find_hardware_h264_encoder() -> Option<IMFTransform> {
    let input_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut activates_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;
    let result = MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        Some(&input_info),
        Some(&output_info),
        &mut activates_ptr,
        &mut count,
    );
    if result.is_err() || activates_ptr.is_null() || count == 0 {
        return None;
    }

    let activates = std::slice::from_raw_parts_mut(activates_ptr, count as usize);
    let mut chosen: Option<IMFTransform> = None;
    for slot in activates.iter_mut() {
        let Some(activate) = slot.take() else { continue };
        if chosen.is_none() {
            match activate.ActivateObject::<IMFTransform>() {
                Ok(t) => chosen = Some(t),
                Err(e) => {
                    crate::logging::log(&format!("hardware H264 MFT activation failed: {e}"));
                }
            }
        }
        // Dropping `activate` here releases the COM reference for every
        // entry, including the one we activated (ActivateObject returns its
        // own separately-refcounted interface pointer).
    }
    windows::Win32::System::Com::CoTaskMemFree(Some(activates_ptr as *const _));
    chosen
}

/// Create + configure the Media Foundation H.264 encoder MFT — a hardware
/// encoder (NVENC/QuickSync/AMF) if one is registered, else the built-in
/// software encoder.
unsafe fn create_encoder(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(IMFTransform, bool, Option<Vec<u8>>), String> {
    let (transform, is_hardware): (IMFTransform, bool) =
        match find_hardware_h264_encoder() {
            Some(t) => {
                crate::logging::log("using hardware H264 encoder MFT");
                (t, true)
            }
            None => {
                crate::logging::log(
                    "no hardware H264 encoder MFT found — falling back to software encoder",
                );
                let t = ctx(
                    CoCreateInstance(&CLSID_CMSH264ENCODER, None, CLSCTX_INPROC_SERVER),
                    "CoCreateInstance H264 encoder",
                )?;
                (t, false)
            }
        };

    // Hardware MFTs are async by contract; unlock that mode explicitly (a
    // no-op, harmless call for the synchronous software encoder, which
    // doesn't have this attribute at all).
    if is_hardware {
        if let Ok(attrs) = transform.GetAttributes() {
            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
        }

        // Hardware MFTs are built around D3D-backed samples; observed
        // behavior on this machine's NVIDIA encoder is that ProcessInput
        // blocks forever (no error, no event) if it never gets a device
        // manager — even when fed plain system-memory samples. Wire it up
        // even though we're not yet handing it GPU surfaces directly
        // (that's the next step: zero-copy DXGI-backed NV12 samples).
        let mut reset_token: u32 = 0;
        let mut device_manager: Option<IMFDXGIDeviceManager> = None;
        match ctx(
            MFCreateDXGIDeviceManager(&mut reset_token, &mut device_manager),
            "MFCreateDXGIDeviceManager",
        ) {
            Ok(()) => match device_manager {
                Some(dm) => match dm.ResetDevice(device, reset_token) {
                    Ok(()) => {
                        let raw = windows::core::Interface::as_raw(&dm) as usize;
                        if let Err(e) =
                            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw)
                        {
                            crate::logging::log(&format!(
                                "SET_D3D_MANAGER failed (continuing without it): {e}"
                            ));
                        }
                    }
                    Err(e) => crate::logging::log(&format!(
                        "IMFDXGIDeviceManager::ResetDevice failed (continuing without it): {e}"
                    )),
                },
                None => crate::logging::log(
                    "MFCreateDXGIDeviceManager produced no device manager (continuing without it)",
                ),
            },
            Err(e) => crate::logging::log(&format!(
                "MFCreateDXGIDeviceManager failed (continuing without it): {e}"
            )),
        }
    }

    let bitrate = ((width * height) as u64 * fps as u64 / 15)
        .clamp(2_000_000, 20_000_000) as u32;

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
    ctx(out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video), "out major")?;
    ctx(out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264), "out subtype")?;
    ctx(out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate), "out bitrate")?;
    ctx(
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
        "out interlace",
    )?;
    ctx(
        out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32),
        "out profile",
    )?;
    ctx(set_size(&out_type, &MF_MT_FRAME_SIZE, width, height), "out size")?;
    ctx(set_ratio(&out_type, &MF_MT_FRAME_RATE, fps, 1), "out rate")?;
    ctx(set_ratio(&out_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1), "out par")?;
    ctx(transform.SetOutputType(0, &out_type, 0), "SetOutputType")?;

    let in_type = ctx(MFCreateMediaType(), "MFCreateMediaType in")?;
    ctx(in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video), "in major")?;
    ctx(in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12), "in subtype")?;
    ctx(
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
        "in interlace",
    )?;
    ctx(set_size(&in_type, &MF_MT_FRAME_SIZE, width, height), "in size")?;
    ctx(set_ratio(&in_type, &MF_MT_FRAME_RATE, fps, 1), "in rate")?;
    ctx(set_ratio(&in_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1), "in par")?;
    ctx(transform.SetInputType(0, &in_type, 0), "SetInputType")?;

    // SPS/PPS for the CodecConfig control message, if the encoder exposes it.
    let codec_config = read_sequence_header(&out_type);

    ctx(
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0),
        "NOTIFY_BEGIN_STREAMING",
    )?;
    ctx(
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0),
        "NOTIFY_START_OF_STREAM",
    )?;

    Ok((transform, is_hardware, codec_config))
}

unsafe fn read_sequence_header(out_type: &IMFMediaType) -> Option<Vec<u8>> {
    let size = out_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    out_type
        .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, None)
        .ok()?;
    Some(buf)
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

/// BGRA (from Desktop Duplication) → NV12 (what the encoder wants), BT.601 limited.
/// Convert (and, when `dst` is smaller than `src`, nearest-neighbor downscale)
/// a captured BGRA desktop frame to NV12 for the encoder.
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

/// Mean of the Y plane, sampled sparsely for speed. Near 16 means the frame
/// fed to the encoder is (still) solid black — the giveaway for a stuck or
/// misdirected capture, vs. a real desktop image which is essentially never
/// uniformly black.
fn average_luma(nv12: &[u8], width: u32, height: u32) -> u32 {
    let y_plane_len = (width * height) as usize;
    let y_plane = &nv12[..y_plane_len.min(nv12.len())];
    if y_plane.is_empty() {
        return 0;
    }
    let stride = (y_plane.len() / 4096).max(1);
    let mut sum = 0u64;
    let mut n = 0u64;
    for &b in y_plane.iter().step_by(stride) {
        sum += b as u64;
        n += 1;
    }
    (sum / n.max(1)) as u32
}
