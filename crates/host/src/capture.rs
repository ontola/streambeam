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
    pub fn new(fps: u32) -> Result<Self, String> {
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32, Option<Vec<u8>>), String>>();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<Option<EncodedFrame>>(4);
        let force_idr = Arc::new(AtomicBool::new(false));
        let thread_force = force_idr.clone();

        let thread = std::thread::Builder::new()
            .name("streambeam-capture".into())
            .spawn(move || run_pipeline(fps, init_tx, req_rx, frame_tx, thread_force))
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
    init_tx: Sender<Result<(u32, u32, Option<Vec<u8>>), String>>,
    req_rx: Receiver<Request>,
    frame_tx: SyncSender<Option<EncodedFrame>>,
    force_idr: Arc<AtomicBool>,
) {
    let mut pipe = match Pipeline::new(fps) {
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
                    eprintln!("capture/encode error: {e}");
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
    width: u32,
    height: u32,
    fps: u32,
    codec_config: Option<Vec<u8>>,
    nv12: Vec<u8>,
    have_last: bool,
    frame_index: i64,
}

fn ctx<T>(r: windows::core::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

impl Pipeline {
    fn new(fps: u32) -> Result<Self, String> {
        unsafe {
            // MTA so the objects can be used from this thread freely.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            ctx(MFStartup(MF_VERSION, MFSTARTUP_FULL), "MFStartup")?;

            let (device, context) = create_d3d_device()?;
            let (duplication, width, height) = create_duplication(&device)?;
            let staging = create_staging(&device, width, height)?;
            let (transform, codec_config) = create_encoder(width, height, fps)?;

            Ok(Self {
                device,
                context,
                duplication,
                staging,
                transform,
                width,
                height,
                fps,
                codec_config,
                nv12: vec![0u8; (width * height * 3 / 2) as usize],
                have_last: false,
                frame_index: 0,
            })
        }
    }

    /// Grab the newest desktop frame (or reuse the last if unchanged), encode it,
    /// and return the resulting access unit (None while the encoder primes).
    fn capture_and_encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        unsafe {
            self.capture_into_nv12()?;
            self.encode(force_idr)
        }
    }

    unsafe fn capture_into_nv12(&mut self) -> Result<(), String> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match self
            .duplication
            .AcquireNextFrame(100, &mut info, &mut resource)
        {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
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
            let src =
                std::slice::from_raw_parts(mapped.pData as *const u8, stride * self.height as usize);
            bgra_to_nv12(src, stride, self.width, self.height, &mut self.nv12);
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

        ctx(self.transform.ProcessInput(0, &sample, 0), "ProcessInput")?;
        self.drain_output()
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

        let keyframe = sample
            .GetUINT32(&MFSampleExtension_CleanPoint)
            .map(|v| v == 1)
            .unwrap_or(false);

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

/// Create + configure the Media Foundation H.264 encoder MFT.
unsafe fn create_encoder(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(IMFTransform, Option<Vec<u8>>), String> {
    let transform: IMFTransform = ctx(
        CoCreateInstance(&CLSID_CMSH264ENCODER, None, CLSCTX_INPROC_SERVER),
        "CoCreateInstance H264 encoder",
    )?;

    let bitrate = ((width * height) as u64 * fps as u64 / 15)
        .clamp(2_000_000, 20_000_000) as u32;

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

    // Low-latency, 1s keyframe interval — best effort (ignore if unsupported).
    let _ = set_codec_bool(&transform, &CODECAPI_AVLOWLATENCYMODE, true);
    let _ = set_codec_u32(&transform, &CODECAPI_RATECONTROL, 0); // CBR
    let _ = set_codec_u32(&transform, &CODECAPI_MEANBITRATE, bitrate);
    let _ = set_codec_u32(&transform, &CODECAPI_GOPSIZE, fps.max(1));

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

    Ok((transform, codec_config))
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
fn bgra_to_nv12(src: &[u8], stride: usize, width: u32, height: u32, out: &mut [u8]) {
    let w = width as usize;
    let h = height as usize;
    let y_plane = w * h;

    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            let p = row + x * 4;
            let b = src[p] as i32;
            let g = src[p + 1] as i32;
            let r = src[p + 2] as i32;
            let yy = (66 * r + 129 * g + 25 * b + 128 >> 8) + 16;
            out[y * w + x] = yy.clamp(0, 255) as u8;
        }
    }

    // Interleaved UV plane at half resolution (sample top-left of each 2×2 block).
    let mut uv = y_plane;
    for y in (0..h).step_by(2) {
        let row = y * stride;
        for x in (0..w).step_by(2) {
            let p = row + x * 4;
            let b = src[p] as i32;
            let g = src[p + 1] as i32;
            let r = src[p + 2] as i32;
            let u = (-38 * r - 74 * g + 112 * b + 128 >> 8) + 128;
            let v = (112 * r - 94 * g - 18 * b + 128 >> 8) + 128;
            out[uv] = u.clamp(0, 255) as u8;
            out[uv + 1] = v.clamp(0, 255) as u8;
            uv += 2;
        }
    }
}
