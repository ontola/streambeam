// This module is Win32/COM/vendor-SDK FFI glue — same conventions as capture.rs.
#![allow(unsafe_op_in_unsafe_fn)]

//! Direct NVIDIA NVENC SDK (`nvEncodeAPI.h`) H.264 encoding — bypasses Windows
//! Media Foundation entirely.
//!
//! Why: the Media Foundation hardware-encoder-MFT path (see the removed
//! MF-hardware code this replaced) got a real NVIDIA hardware encoder working
//! *most* of the time, but hit an intermittent, unrecoverable silent freeze
//! after a variable number of frames (confirmed via a live `procdump`+`cdb`
//! stack trace: the capture thread parked on a normal channel wait, not
//! blocked inside any MF/driver call — i.e. genuinely lost, not just slow).
//! Researched how LizardByte/Sunshine and RustDesk actually do this on
//! Windows: **neither goes through Media Foundation's hardware MFT wrapper
//! for NVIDIA**. Sunshine calls the NVENC SDK directly (register a real
//! `ID3D11Texture2D` via `NvEncRegisterResource`, one process-wide completion
//! event, a hard ~100ms per-frame timeout that drops the frame instead of
//! blocking). This module follows that pattern.
//!
//! Struct/enum layouts and GUIDs below are adapted from the MIT-licensed
//! `nvenc` crate (https://github.com/AlsoSylv/nvenc, Copyright 2025 AlsoSylv)
//! as a reference for the correct NVENC ABI — not used as a dependency,
//! because its async-completion-event support has a real bug (wrong function
//! pointer signature, and not exposed publicly), which is exactly the piece
//! this module needs. Fields we don't set ourselves are left as opaque
//! reserved blobs (`[u32; N]`) rather than fully modeled, since we only ever
//! read/write a handful of top-level fields — everything codec-specific
//! (`NV_ENC_CONFIG_H264`, etc.) comes back pre-populated from
//! `NvEncGetEncodePresetConfigEx` and we never touch it.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{GUID, Interface};

use crate::encode::EncodedFrame;

const NVENC_DLL: &str = "nvEncodeAPI64.dll";
const NVENC_MAJOR_VERSION: u32 = 13;
const NVENC_MINOR_VERSION: u32 = 1;
const NVENC_API_VERSION: u32 = NVENC_MAJOR_VERSION | (NVENC_MINOR_VERSION << 24);

const fn struct_version(api_struct_version: u32) -> u32 {
    NVENC_API_VERSION | (api_struct_version << 16) | (0x7 << 28)
}

const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
const NV_ENC_PRESET_CONFIG_VER: u32 = struct_version(5) | (1 << 31);
const NV_ENC_CONFIG_VER: u32 = struct_version(9) | (1 << 31);
const NV_ENC_INITIALIZE_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
const NV_ENC_REGISTER_RESOURCE_VER: u32 = struct_version(5);
const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
const NV_ENC_PIC_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
const NV_ENC_LOCK_BITSTREAM_VER: u32 = struct_version(2) | (1 << 31);
const NV_ENC_EVENT_PARAMS_VER: u32 = struct_version(2);
const NVENC_API_LIST_VERSION: u32 = struct_version(2);
const NV_ENC_RECONFIGURE_PARAMS_VER: u32 = struct_version(1) | (1 << 31);

const NV_ENC_CODEC_H264_GUID: GUID = GUID::from_values(
    0x6bc82762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
const NV_ENC_PRESET_P4_GUID: GUID = GUID::from_values(
    0x90a7b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);
// Byte values cross-checked against the authoritative FFmpeg nv-codec-headers
// nvEncodeAPI.h (not a secondhand reference crate — see this file's other
// GUIDs' history of exactly this class of bug).
const NV_ENC_H264_PROFILE_HIGH_GUID: GUID = GUID::from_values(
    0xe7cbc309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
// AV1 support (Ada Lovelace/RTX 40-series and later). Unlike the struct
// layouts elsewhere in this file, a GUID is just a 16-byte opaque comparison
// key, not a memory-layout-critical field order — if either of these is
// wrong, `NvEncGetEncodePresetConfigEx`/`NvEncInitializeEncoder` simply
// return a "not supported" error (handled by falling back to H.264 in
// `capture.rs`), not corruption. Not independently re-verified against the
// authoritative header the way the H.264 GUIDs above were (no AV1-capable
// GPU used for that cross-check) — if AV1 never initializes on a GPU that
// should support it, check these first.
const NV_ENC_CODEC_AV1_GUID: GUID = GUID::from_values(
    0x0a352289,
    0x0aa7,
    0x4759,
    [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
);
const NV_ENC_AV1_PROFILE_MAIN_GUID: GUID = GUID::from_values(
    0x5f2a39f5,
    0xf14e,
    0x4f95,
    [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
);

use crate::encode::Codec;

/// Selects the codec/profile GUIDs `NvencEncoder::new` initializes with, and
/// which bitstream format `encode` parses its own output as (Annex-B NALs
/// vs AV1 OBUs) for keyframe detection.
impl Codec {
    fn encode_guid(self) -> GUID {
        match self {
            Codec::H264 => NV_ENC_CODEC_H264_GUID,
            Codec::Av1 => NV_ENC_CODEC_AV1_GUID,
        }
    }

    fn profile_guid(self) -> GUID {
        match self {
            Codec::H264 => NV_ENC_H264_PROFILE_HIGH_GUID,
            Codec::Av1 => NV_ENC_AV1_PROFILE_MAIN_GUID,
        }
    }

    fn is_keyframe(self, bytes: &[u8]) -> bool {
        match self {
            Codec::H264 => streambeam_core::access_unit_is_keyframe(bytes),
            Codec::Av1 => streambeam_core::av1_access_unit_is_keyframe(bytes),
        }
    }
}

// --- enums (only the values we actually use) ---
const NVENC_DEVICE_TYPE_DIRECTX: u32 = 0x0;
const NVENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0x0;
const NVENC_BUFFER_FORMAT_ARGB: u32 = 0x01000000; // matches DXGI_FORMAT_B8G8R8A8_UNORM's byte order
/// 10-bit packed A2B10G10R10 — matches `DXGI_FORMAT_R10G10B10A2_UNORM`'s
/// channel order (R in the low bits), the HDR10 passthrough render target.
/// Value from nvEncodeAPI.h 13.1 (`NV_ENC_BUFFER_FORMAT_ABGR10`).
const NVENC_BUFFER_FORMAT_ABGR10: u32 = 0x20000000;

/// `NV_ENC_BIT_DEPTH_10`.
const NVENC_BIT_DEPTH_10: u32 = 10;
/// H.273 / ISO-IEC 23091-4 code points for HDR10, as NVENC's VUI enums.
const NVENC_VUI_COLOR_PRIMARIES_BT2020: u32 = 9;
const NVENC_VUI_TRANSFER_SMPTE2084: u32 = 16;
const NVENC_VUI_MATRIX_BT2020_NCL: u32 = 9;

/// `u32` indices into `NV_ENC_CONFIG_AV1`, verified by compiling
/// nv-codec-headers 13.1 (the version these bindings target) and printing
/// `offsetof` — the same cross-checking discipline the `idrPeriod` and
/// `repeatSeqHdr` offsets below were established with, and `idrPeriod` coming
/// out at index 5 confirms the method agrees with what was proven in the field.
mod av1_cfg {
    pub const COLOR_PRIMARIES: usize = 17;
    pub const TRANSFER_CHARACTERISTICS: usize = 18;
    pub const MATRIX_COEFFICIENTS: usize = 19;
    pub const COLOR_RANGE: usize = 20;
    pub const OUTPUT_BIT_DEPTH: usize = 28;
    pub const INPUT_BIT_DEPTH: usize = 29;
}
const NVENC_BUFFER_USAGE_IMAGE: u32 = 0x0;
const NVENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
const NVENC_PARAMS_RC_MODE_CBR: u32 = 0x02;
const NVENC_PIC_STRUCT_FRAME: u32 = 0x01;
const NVENC_PIC_TYPE_AUTOSELECT: u32 = 0x00; // NV_ENC_PIC_TYPE::P used as "let rate control decide"; IDR forced separately

// --- structs (exact NVENC ABI layout; fields we never touch stay `rsvd*`) ---

#[repr(C)]
#[derive(Clone, Copy)]
struct OpenEncodeSessionExParams {
    version: u32,
    device_type: u32,
    device: *mut c_void,
    rsvd: *mut c_void,
    api_version: u32,
    rsvd1: [u32; 253],
    rsvd2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RcParams {
    version: u32,
    rate_control_mode: u32,
    const_qp: [u32; 3],
    average_bit_rate: u32,
    max_bit_rate: u32,
    vbv_buffer_size: u32,
    vbv_initial_delay: u32,
    bit_fields: u32,
    min_qp: [u32; 3],
    max_qp: [u32; 3],
    initial_rcqp: [u32; 3],
    temporal_layer_idx_mask: u32,
    temporal_layer_qp: [u8; 8],
    target_quality: u8,
    target_quality_lsb: u8,
    look_ahead_depth: u16,
    low_delay_key_frame_scale: u8,
    y_dc_qp_index_offset: i8,
    u_dc_qp_index_offset: i8,
    v_dc_qp_index_offset: i8,
    qp_map_mode: u32,
    multi_pass: u32,
    alpha_layer_bitrate_ratio: u32,
    cb_qp_index_offset: i8,
    cr_qp_index_offset: i8,
    rsvd2: u16,
    lookahead_level: u32,
    view_bitrate_ratios: [u8; 7],
    rsvd3: u8,
    rsvd1: u32,
}

// `encode_codec_config` is a union whose largest documented member (`rsvd`)
// is `[u32; 320]` — we never construct or modify it ourselves (it comes
// pre-populated from `NvEncGetEncodePresetConfigEx`), so an opaque
// same-sized blob is ABI-correct without modeling every codec's config.
#[repr(C)]
#[derive(Clone, Copy)]
struct Config {
    version: u32,
    profile_guid: GUID,
    gop_len: u32,
    frame_interval_p: i32,
    mono_chrome_encoding: u32,
    frame_field_mode: u32,
    mv_precision: u32,
    rc_params: RcParams,
    encode_codec_config: [u32; 320],
    rsvd: [u32; 278],
    rsvd2: [*mut c_void; 64],
}

#[repr(C)]
struct PresetConfig {
    version: u32,
    rsvd: u32,
    preset_cfg: Config,
    rsvd1: [u32; 256],
    rsvd2: [*mut c_void; 64],
}

#[repr(C)]
struct InitializeParams {
    version: u32,
    encode_guid: GUID,
    preset_guid: GUID,
    encode_width: u32,
    encode_height: u32,
    dar_width: u32,
    dar_height: u32,
    frame_rate_num: u32,
    frame_rate_den: u32,
    enable_encode_async: u32,
    enable_ptd: u32,
    bit_flags: u32,
    priv_data_size: u32,
    rsvd: u32,
    priv_data: *mut c_void,
    encode_config: *mut Config,
    max_encode_width: u32,
    max_encode_height: u32,
    // NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE[2] — each element is a
    // 4-u32 struct (bitfields + `reserved1[3]`), so this is 8 u32s, not 4.
    max_me_hint_counts_per_block: [u32; 8],
    tuning_info: u32,
    buffer_format: u32,
    num_state_buffers: u32,
    output_stats_level: u32,
    rsvd1: [u32; 284],
    rsvd2: [*mut c_void; 64],
}

#[repr(C)]
struct RegisterResource {
    version: u32,
    resource_type: u32,
    width: u32,
    height: u32,
    pitch: u32,
    sub_resource_index: u32,
    resource_to_register: *mut c_void,
    registered_resource: *mut c_void,
    buffer_format: u32,
    buffer_usage: u32,
    input_fence_point: *mut c_void,
    chroma_offset: [u32; 2],
    chroma_offset_in: [u32; 2],
    rsvd1: [u32; 244],
    rsvd2: [*mut c_void; 61],
}

#[repr(C)]
struct MapInputResource {
    version: u32,
    sub_resource_idx: u32,
    input_resource: *mut c_void,
    registered_resource: *mut c_void,
    mapped_resource: *mut c_void,
    mapped_buffer_format: u32,
    rsvd1: [u32; 251],
    rsvd: [*mut c_void; 63],
}

#[repr(C)]
struct CreateBitstreamBuffer {
    version: u32,
    size: u32,
    memory_heap: u32,
    rsvd: u32,
    bitstream_buffer: *mut c_void,
    bitstream_buffer_ptr: *mut c_void,
    rsvd1: [u32; 58],
    rsvd2: [*mut c_void; 64],
}

// `codec_pic_params` (H264/HEVC/AV1 union) left opaque — a fresh
// zero-initialized union is valid for all three (no SEI/LTR/slice-mode
// extras), so we don't need to model `NV_ENC_PIC_PARAMS_H264` at all.
#[repr(C)]
struct PicParams {
    version: u32,
    input_width: u32,
    input_height: u32,
    input_pitch: u32,
    encode_pic_flags: u32,
    frame_idx: u32,
    input_time_stamp: u64,
    input_duration: u64,
    input_buffer: *mut c_void,
    output_bitstream: *mut c_void,
    completion_event: *mut c_void,
    buffer_format: u32,
    picture_struct: u32,
    picture_type: u32,
    codec_pic_params: [u32; 256],
    // Same NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE[2] layout as
    // `InitializeParams::max_me_hint_counts_per_block` — 8 u32s, not 4.
    me_hint_counts_per_block: [u32; 8],
    me_external_hints: *mut c_void,
    rsvd2: [u32; 7],
    rsvd5: [*mut c_void; 2],
    qp_delta_map: *mut i8,
    qp_delta_map_size: u32,
    rsvd_bit_fields: u32,
    me_hint_ref_pic_dist: [u16; 2],
    rsvd4: u32,
    alpha_buffer: *mut c_void,
    me_external_sb_hints: *mut c_void,
    me_sb_hints_count: u32,
    state_buffer_idx: u32,
    output_recon_buffer: *mut c_void,
    rsvd3: [u32; 284],
    rsvd6: [*mut c_void; 57],
}

// `NV_ENC_PIC_FLAG_FORCEIDR` per the real NVENC SDK header — 0x2, NOT 0x1
// (0x1 is `NV_ENC_PIC_FLAG_FORCEINTRA`: an all-intra frame that is NOT a
// real IDR — no SPS/PPS repeat, no NAL type 5, doesn't reset the decoder's
// reference chain). Using 0x1 here meant every "forced keyframe" after the
// session's very first frame — which NVENC always makes a real IDR
// automatically regardless of any flag — was actually just a large
// all-intra P-slice. Invisible under lossless testing (nothing ever needed
// resyncing), catastrophic under real packet loss: no genuine recovery
// point ever existed past frame 0, so any loss became permanent.
const PIC_FLAG_FORCEIDR: u32 = 0x2;

#[repr(C)]
struct LockBitstream {
    version: u32,
    bit_fields: u32,
    output_bit_stream: *mut c_void,
    slice_offsets: *mut u32,
    frame_idx: u32,
    hw_encode_status: u32,
    num_slices: u32,
    bitstream_size_in_bytes: u32,
    output_time_stamp: u64,
    output_duration: u64,
    bitstream_buffer_ptr: *mut c_void,
    picture_type: u32,
    picture_struct: u32,
    frame_avg_qp: u32,
    frame_satd: u32,
    ltr_frame_idx: u32,
    ltr_frame_bitmap: u32,
    temporal_id: u32,
    intra_mb_count: u32,
    inter_mb_count: u32,
    average_mvx: i32,
    average_mvy: i32,
    alpha_layer_size_in_bytes: u32,
    output_stats_ptr_size: u32,
    rsvd: u32,
    output_stats_ptr: *mut c_void,
    frame_idx_display: u32,
    rsvd1: [u32; 219],
    rsvd2: [*mut c_void; 63],
    rsvd_internal: [u32; 8],
}

#[repr(C)]
struct EventParams {
    version: u32,
    rsvd: u32,
    completion_event: *mut c_void,
    rsvd1: [u32; 254],
    rsvd2: [*mut c_void; 64],
}

// --- function table ---
// Slot order and count must match the real NVENC ABI exactly (this is a
// flat C function-pointer table filled in by `NvEncodeAPICreateInstance` —
// any drift here silently corrupts every call after the first mismatch).
// Entries we never call are typed as `*mut c_void` placeholders purely to
// reserve their slot; the ones we do call get real signatures. This fixes
// the specific bug found in the reference `nvenc` crate: its
// `nvenc_register_async_event`/`nvenc_unregister_async_event` slots were
// declared as `fn(*mut c_void) -> NVencResult`, missing the
// `*mut NV_ENC_EVENT_PARAMS` argument the real ABI takes.
type OpenEncodeSessionExFn =
    unsafe extern "C" fn(*mut OpenEncodeSessionExParams, *mut *mut c_void) -> i32;
type GetEncodePresetConfigExFn =
    unsafe extern "C" fn(*mut c_void, GUID, GUID, u32, *mut MaybeUninit<PresetConfig>) -> i32;
type InitializeEncoderFn = unsafe extern "C" fn(*mut c_void, *mut InitializeParams) -> i32;
type RegisterResourceFn = unsafe extern "C" fn(*mut c_void, *mut RegisterResource) -> i32;
type UnregisterResourceFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type MapInputResourceFn = unsafe extern "C" fn(*mut c_void, *mut MapInputResource) -> i32;
type UnmapInputResourceFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type CreateBitstreamBufferFn = unsafe extern "C" fn(*mut c_void, *mut CreateBitstreamBuffer) -> i32;
type DestroyBitstreamBufferFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type EncodePictureFn = unsafe extern "C" fn(*mut c_void, *mut PicParams) -> i32;
type LockBitstreamFn = unsafe extern "C" fn(*mut c_void, *mut LockBitstream) -> i32;
type UnlockBitstreamFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type RegisterAsyncEventFn = unsafe extern "C" fn(*mut c_void, *mut EventParams) -> i32;
type UnregisterAsyncEventFn = unsafe extern "C" fn(*mut c_void, *mut EventParams) -> i32;
type DestroyEncoderFn = unsafe extern "C" fn(*mut c_void) -> i32;
type ReconfigureEncoderFn = unsafe extern "C" fn(*mut c_void, *mut ReconfigureParams) -> i32;
// `NvEncInvalidateRefFrames(void* encoder, uint64_t invalidRefFrameTimeStamp)`
// — the timestamp is the `PicParams::input_time_stamp` the frame was encoded
// with (echoed back in `LockBitstream::output_time_stamp`).
type InvalidateRefFramesFn = unsafe extern "C" fn(*mut c_void, u64) -> i32;

// Layout cross-checked against the authoritative NVIDIA SDK header
// (Samples/NvCodec/NvEncoder/nvEncodeAPI.h, `_NV_ENC_RECONFIGURE_PARAMS`):
// version, then the full `NV_ENC_INITIALIZE_PARAMS` embedded *by value* (not
// a pointer), then a packed bitfield `resetEncoder:1, forceIDR:1,
// reserved:30` — LSB-first, matching this file's other bitfield reads.
#[repr(C)]
struct ReconfigureParams {
    version: u32,
    re_init_encode_params: InitializeParams,
    bit_fields: u32,
}

#[repr(C)]
struct FunctionList {
    version: u32,
    rsvd: u32,
    open_encode_session: *mut c_void,
    get_encode_guid_count: *mut c_void,
    get_encode_profile_guid_count: *mut c_void,
    get_encode_profile_guids: *mut c_void,
    get_encode_guids: *mut c_void,
    get_input_format_count: *mut c_void,
    get_input_formats: *mut c_void,
    get_encode_caps: *mut c_void,
    get_encode_preset_count: *mut c_void,
    get_encode_preset_guids: *mut c_void,
    get_encode_preset_config: *mut c_void,
    initialize_encoder: InitializeEncoderFn,
    create_input_buffer: *mut c_void,
    destroy_input_buffer: *mut c_void,
    create_bitstream_buffer: CreateBitstreamBufferFn,
    destroy_bitstream_buffer: DestroyBitstreamBufferFn,
    encode_picture: EncodePictureFn,
    lock_bitstream: LockBitstreamFn,
    unlock_bitstream: UnlockBitstreamFn,
    lock_input_buffer: *mut c_void,
    unlock_input_buffer: *mut c_void,
    get_encode_stats: *mut c_void,
    get_sequence_params: *mut c_void,
    register_async_event: RegisterAsyncEventFn,
    unregister_async_event: UnregisterAsyncEventFn,
    map_input_resource: MapInputResourceFn,
    unmap_input_resource: UnmapInputResourceFn,
    destroy_encoder: DestroyEncoderFn,
    invalidate_ref_frames: InvalidateRefFramesFn,
    open_encode_session_ex: OpenEncodeSessionExFn,
    register_resource: RegisterResourceFn,
    unregister_resource: UnregisterResourceFn,
    reconfigure_encoder: ReconfigureEncoderFn,
    rsvd1: *mut c_void,
    create_mv_buffer: *mut c_void,
    destroy_mv_buffer: *mut c_void,
    run_motion_estimation_only: *mut c_void,
    get_last_error: *mut c_void,
    set_io_cuda_streams: *mut c_void,
    get_encode_preset_config_ex: GetEncodePresetConfigExFn,
    get_sequence_params_ex: *mut c_void,
    store_encoder_state: *mut c_void,
    look_ahead_picture: *mut c_void,
    rsvd2: [*mut c_void; 275],
}

type CreateInstanceFn = unsafe extern "C" fn(*mut MaybeUninit<FunctionList>) -> i32;

fn err(step: &str, code: i32) -> String {
    format!("NVENC {step} failed (code {code})")
}

fn check(step: &str, code: i32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(err(step, code))
    }
}

/// Direct NVENC encoder — one D3D11 texture registered once at construction,
/// re-used (via `CopyResource` from the caller) every frame. No CPU touches
/// the pixel data at any point.
pub struct NvencEncoder {
    encoder: NonNull<c_void>,
    functions: FunctionList,
    bitstream_buffer: *mut c_void,
    registered_resource: *mut c_void,
    completion_event: HANDLE,
    width: u32,
    height: u32,
    fps: u32,
    frame_index: i64,
    codec: Codec,
    /// HDR10 passthrough: input and output are 10-bit PQ/BT.2020. Fixed for
    /// the encoder's lifetime — it determines the registered resource's format.
    hdr10: bool,
}

// The encoder handle + function table are just data + a DLL's exported
// function pointers; NVENC itself documents the session as safe to drive
// from a single dedicated thread, which is how `capture.rs` already uses it
// (same thread that owns the D3D11 device/context).
unsafe impl Send for NvencEncoder {}

/// Builds the codec config used both at session creation (`NvencEncoder::new`)
/// and on a live bitrate/IDR reconfigure (`NvencEncoder::reconfigure`) —
/// shared so the two paths can't drift apart on what "the same encoder,
/// different bitrate" actually means.
unsafe fn build_config(
    functions: &FunctionList,
    encoder: NonNull<c_void>,
    codec: Codec,
    fps: u32,
    bitrate_bps: u32,
    hdr10: bool,
) -> Result<Config, String> {
    let mut preset_uninit: MaybeUninit<PresetConfig> = MaybeUninit::zeroed();
    (*preset_uninit.as_mut_ptr()).version = NV_ENC_PRESET_CONFIG_VER;
    (*preset_uninit.as_mut_ptr()).preset_cfg.version = NV_ENC_CONFIG_VER;
    check(
        "NvEncGetEncodePresetConfigEx",
        (functions.get_encode_preset_config_ex)(
            encoder.as_ptr(),
            codec.encode_guid(),
            NV_ENC_PRESET_P4_GUID,
            NVENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            &mut preset_uninit,
        ),
    )?;
    let mut config = preset_uninit.assume_init().preset_cfg;
    // Override only what we care about; every other field (including
    // the whole codec-specific H264/AV1 sub-config) stays exactly what
    // the ultra-low-latency preset already set, which for H.264 already
    // enables SPS/PPS repetition on every IDR — required since our
    // client parses SPS/PPS straight out of each keyframe's Annex-B
    // bytes.
    // Periodic keyframe as a *safety net*, not the recovery mechanism —
    // loss recovery is ref-invalidation plus client-requested IDRs (both
    // land within ~RTT), so this only bounds how long an undetected decoder
    // drift can last. Stretched from 3s: a CBR intra frame at streaming
    // bitrates is ~0.1 bits/pixel and visibly banded on gradients, so every
    // periodic keyframe reads as a brief "256-color" contrast flash that
    // the following P-frames refine away (user-reported 2026-07-19); at 10s
    // the pulse is rare instead of metronomic.
    config.gop_len = fps.max(1) * 10;
    config.frame_interval_p = 1; // no B-frames — lowest latency
    // Was forced to Baseline (no CABAC) to match the software encoder's
    // known-good `eAVEncH264VProfile_Base` while debugging decode issues
    // on real Android hardware decoders — that debugging need has passed
    // and NVENC is now the primary path, so pay CABAC's real
    // quality-per-bit cost back: at the same CBR bitrate, Baseline's
    // CAVLC-only entropy coding visibly softens detail and produces more
    // motion/text artifacts than High profile, which effectively every
    // Android device since H.264 was relevant decodes fine. `frame_interval_p
    // = 1` above already keeps it B-frame-free regardless of profile, so
    // this doesn't reintroduce the extra reference-frame latency High
    // profile can otherwise imply.
    config.profile_guid = codec.profile_guid();
    config.rc_params.rate_control_mode = NVENC_PARAMS_RC_MODE_CBR;
    config.rc_params.average_bit_rate = bitrate_bps;
    // `max_bit_rate`/`vbv_buffer_size` equal to the average left zero
    // burst headroom: a sudden complexity spike (e.g. a UI hover
    // highlight) had nowhere to borrow extra bits from and got
    // hard-quantized instead, visible as a brief contrast crush even
    // with spatial AQ on (AQ only reallocates bits *within* a frame's
    // budget — it can't manufacture bits a zero-headroom VBV forbids).
    // Giving 50% headroom on the ceiling and ~2x the buffer lets a
    // one-off spike produce a bigger frame instead of getting crushed.
    // This does NOT add glass-to-glass latency in this pipeline: there
    // are no B-frames (frame_interval_p = 1) and no segment/jitter
    // buffering downstream — every frame is still sent the instant
    // it's encoded, over our own QUIC/FEC transport. VBV buffer size
    // only bounds how large a single frame is allowed to get before the
    // rate controller clamps it, not when frames are emitted.
    config.rc_params.max_bit_rate = bitrate_bps + bitrate_bps / 2;
    config.rc_params.vbv_buffer_size = bitrate_bps * 2;
    config.rc_params.vbv_initial_delay = bitrate_bps * 2;
    // Spatial AQ (`enableAQ`, bit 3 of `NV_ENC_RC_PARAMS`'s packed
    // bitfield — layout cross-checked against the authoritative FFmpeg
    // nv-codec-headers nvEncodeAPI.h, LSB-first matching declared field
    // order: enableMinQP=0, enableMaxQP=1, enableInitialRCQP=2,
    // enableAQ=3). Without it, single-pass CBR picks one QP for the
    // *whole* frame from its aggregate complexity estimate — a small
    // localized change (a button's hover-highlight) shifts that
    // estimate enough to visibly shift the entire frame's quantization,
    // seen as the whole screen flashing/breathing in brightness on
    // every such change. Spatial AQ lets QP vary per macroblock instead,
    // so the small complex region gets the extra bits it needs without
    // forcing every other macroblock to re-quantize that frame. Aka why
    // every screen-capture NVENC tool (Sunshine included) enables this.
    config.rc_params.bit_fields |= 1 << 3;
    // AV1's SPS/PPS-repetition equivalent, `repeatSeqHdr` ("output Sequence
    // Header for every Key frame"), is NOT a preset default the way H.264's
    // `repeatSPSPPS` is — proven in the field 2026-07-18: a ~50-minute av01
    // session with `gop_len = fps*3` logged zero `keyframe=true` frames,
    // because both ends detect an AV1 keyframe by sequence-header-OBU
    // presence (`obu.rs::access_unit_is_keyframe`), and without this bit
    // NVENC only emits the sequence header on a session's very first frame.
    // Every downstream keyframe consumer (client behind-skip, loss resync,
    // host keyframe logging) silently degraded. Layout cross-checked against
    // nv-codec-headers nvEncodeAPI.h `_NV_ENC_CONFIG_AV1`: four leading u32s
    // (level, tier, minPartSize, maxPartSize), then the LSB-first packed
    // bitfield word — outputAnnexBFormat=0, enableTimingInfo=1,
    // enableDecoderModelInfo=2, enableFrameIdNumbers=3, disableSeqHdr=4,
    // repeatSeqHdr=5.
    if codec == Codec::Av1 {
        config.encode_codec_config[4] |= 1 << 5;
    }
    // The codec-specific `idrPeriod` must be set to match: the top-level
    // `gopLength` only becomes the IDR interval "if [idrPeriod is] not set"
    // (nvEncodeAPI.h), and the ultra-low-latency preset DOES set it — to
    // NVENC_INFINITE_GOPLENGTH, the header's own suggestion for low-latency
    // clients. Proven live 2026-07-18 (AV1): 650+ frames after a forced IDR
    // with `gop_len = 360` and not one periodic keyframe — so any decoder
    // drift (client-local datagram loss, an imperfect ref invalidation)
    // was *permanent* between forced IDRs. Offsets cross-checked against
    // nvEncodeAPI.h: NV_ENC_CONFIG_AV1 = level, tier, minPartSize,
    // maxPartSize, bitfield word, idrPeriod → u32 index 5;
    // NV_ENC_CONFIG_H264 = bitfield word, level, idrPeriod → u32 index 2.
    let idr_period_word = match codec {
        Codec::Av1 => 5,
        Codec::H264 => 2,
    };
    config.encode_codec_config[idr_period_word] = config.gop_len;
    if hdr10 {
        configure_hdr10(&mut config, codec)?;
    }
    Ok(config)
}

/// Tell NVENC to encode 10-bit and to *say so* in the bitstream.
///
/// The bit depth and the colour description have to travel together: a decoder
/// handed 10-bit samples with no transfer function will render PQ code values
/// as if they were plain gamma, which looks like the washed-out picture this
/// whole feature exists to fix — just in the other direction. Offsets are the
/// compiler-verified ones in `av1_cfg`.
///
/// AV1 only: H.264 High profile is 8-bit, and the 10-bit alternative
/// (High10) isn't what NVENC's HEVC/AV1 HDR path uses. A caller asking for
/// HDR10 with H.264 is a bug, not a fallback, so it errors rather than
/// silently sending SDR that claims to be HDR.
unsafe fn configure_hdr10(config: &mut Config, codec: Codec) -> Result<(), String> {
    if codec != Codec::Av1 {
        return Err(format!("HDR10 passthrough needs AV1, not {codec:?}"));
    }
    config.encode_codec_config[av1_cfg::INPUT_BIT_DEPTH] = NVENC_BIT_DEPTH_10;
    config.encode_codec_config[av1_cfg::OUTPUT_BIT_DEPTH] = NVENC_BIT_DEPTH_10;
    config.encode_codec_config[av1_cfg::COLOR_PRIMARIES] = NVENC_VUI_COLOR_PRIMARIES_BT2020;
    config.encode_codec_config[av1_cfg::TRANSFER_CHARACTERISTICS] = NVENC_VUI_TRANSFER_SMPTE2084;
    config.encode_codec_config[av1_cfg::MATRIX_COEFFICIENTS] = NVENC_VUI_MATRIX_BT2020_NCL;
    // Studio swing, the HDR10 convention. The SDR AV1 path deliberately
    // doesn't force full range either (NVENC's AV1 output is limited range and
    // states so in-band), so this keeps the two consistent.
    config.encode_codec_config[av1_cfg::COLOR_RANGE] = 0;
    Ok(())
}

impl NvencEncoder {
    /// `texture` must be `D3D11_USAGE_DEFAULT` with `D3D11_BIND_RENDER_TARGET`,
    /// `DXGI_FORMAT_B8G8R8A8_UNORM`, exactly `width`x`height` — the caller
    /// (`capture.rs`) owns it and `CopyResource`s the captured frame into it
    /// each tick; this type only registers it once and never allocates its
    /// own texture.
    pub unsafe fn new(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u32,
        codec: Codec,
        // HDR10 passthrough: 10-bit PQ/BT.2020 in and out, fed from a
        // `DXGI_FORMAT_R10G10B10A2_UNORM` texture. AV1 only — see
        // `configure_hdr10`.
        hdr10: bool,
    ) -> Result<Self, String> {
        let buffer_format = if hdr10 {
            NVENC_BUFFER_FORMAT_ABGR10
        } else {
            NVENC_BUFFER_FORMAT_ARGB
        };
        let lib = libloading::Library::new(NVENC_DLL)
            .map_err(|e| format!("LoadLibrary {NVENC_DLL}: {e}"))?;
        let create_instance: CreateInstanceFn = *lib
            .get(b"NvEncodeAPICreateInstance")
            .map_err(|e| format!("GetProcAddress NvEncodeAPICreateInstance: {e}"))?;
        // Deliberately leaked: the NVENC session must outlive this function,
        // and the DLL is a small, process-lifetime, singleton-ish load
        // (matches how `nvenc_init()` in the reference crate uses a
        // `OnceLock` it never tears down either).
        std::mem::forget(lib);

        let mut functions_uninit: MaybeUninit<FunctionList> = MaybeUninit::zeroed();
        (*functions_uninit.as_mut_ptr()).version = NVENC_API_LIST_VERSION;
        check(
            "NvEncodeAPICreateInstance",
            create_instance(&mut functions_uninit),
        )?;
        let functions = functions_uninit.assume_init();

        let mut session_params: OpenEncodeSessionExParams = std::mem::zeroed();
        session_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        session_params.device_type = NVENC_DEVICE_TYPE_DIRECTX;
        session_params.api_version = NVENC_API_VERSION;
        session_params.device = Interface::as_raw(device);
        let mut encoder_ptr: *mut c_void = std::ptr::null_mut();
        check(
            "NvEncOpenEncodeSessionEx",
            (functions.open_encode_session_ex)(&mut session_params, &mut encoder_ptr),
        )?;
        let encoder = NonNull::new(encoder_ptr).ok_or("NvEncOpenEncodeSessionEx: null encoder")?;

        let mut config = build_config(&functions, encoder, codec, fps, bitrate_bps, hdr10)?;

        let mut init_params: InitializeParams = std::mem::zeroed();
        init_params.version = NV_ENC_INITIALIZE_PARAMS_VER;
        init_params.encode_guid = codec.encode_guid();
        init_params.preset_guid = NV_ENC_PRESET_P4_GUID;
        init_params.encode_width = width;
        init_params.encode_height = height;
        init_params.dar_width = width;
        init_params.dar_height = height;
        init_params.frame_rate_num = fps;
        init_params.frame_rate_den = 1;
        init_params.enable_encode_async = 1;
        init_params.enable_ptd = 1;
        init_params.encode_config = &mut config;
        init_params.max_encode_width = width;
        init_params.max_encode_height = height;
        init_params.tuning_info = NVENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        init_params.buffer_format = buffer_format;
        check(
            "NvEncInitializeEncoder",
            (functions.initialize_encoder)(encoder.as_ptr(), &mut init_params),
        )?;

        let completion_event =
            CreateEventW(None, false, false, None).map_err(|e| format!("CreateEventW: {e}"))?;
        let mut event_params: EventParams = std::mem::zeroed();
        event_params.version = NV_ENC_EVENT_PARAMS_VER;
        event_params.completion_event = completion_event.0;
        check(
            "NvEncRegisterAsyncEvent",
            (functions.register_async_event)(encoder.as_ptr(), &mut event_params),
        )?;

        let mut register_params: RegisterResource = std::mem::zeroed();
        register_params.version = NV_ENC_REGISTER_RESOURCE_VER;
        register_params.resource_type = NVENC_INPUT_RESOURCE_TYPE_DIRECTX;
        register_params.width = width;
        register_params.height = height;
        register_params.resource_to_register = Interface::as_raw(texture);
        register_params.buffer_format = buffer_format;
        register_params.buffer_usage = NVENC_BUFFER_USAGE_IMAGE;
        check(
            "NvEncRegisterResource",
            (functions.register_resource)(encoder.as_ptr(), &mut register_params),
        )?;

        let mut bitstream_params: CreateBitstreamBuffer = std::mem::zeroed();
        bitstream_params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        check(
            "NvEncCreateBitstreamBuffer",
            (functions.create_bitstream_buffer)(encoder.as_ptr(), &mut bitstream_params),
        )?;

        Ok(Self {
            encoder,
            functions,
            bitstream_buffer: bitstream_params.bitstream_buffer,
            registered_resource: register_params.registered_resource,
            completion_event,
            width,
            height,
            fps,
            frame_index: 0,
            codec,
            hdr10,
        })
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether this session encodes HDR10. A recreate must preserve it — the
    /// registered texture is 10-bit and the stream already told the client it
    /// is PQ/BT.2020, so silently dropping to 8-bit SDR mid-session would be
    /// the same "signalled one thing, sent another" failure the colour work
    /// exists to avoid.
    pub fn is_hdr10(&self) -> bool {
        self.hdr10
    }

    /// Live bitrate/IDR reconfigure of this *existing* NVENC session — no
    /// `NvEncDestroyEncoder`/`NvEncOpenEncodeSessionEx` round trip, so no
    /// window where the shared `encode_texture` is briefly unregistered from
    /// any encoder, and no reference-frame-numbering discontinuity for the
    /// decoder to trip over. Replaces the previous "destroy the whole
    /// session and build a fresh one" approach for the two routine triggers
    /// (bitrate change, forced-IDR/recovery request) — that approach worked,
    /// but every recreate was itself disruptive (briefly stalls capture, and
    /// user reports/repro sessions correlated it with visible decode
    /// corruption/contrast flashes), and both triggers fire often enough on
    /// real network conditions that the disruption was closer to constant
    /// than occasional. `resetEncoder=1` resets NVENC's internal rate-control
    /// state and — since `enable_ptd=1` was set at session creation — forces
    /// the next picture to be a real IDR per NVIDIA's own documented
    /// behavior for that flag, giving the same "clean keyframe" guarantee
    /// the old recreate-based approach relied on, without tearing anything
    /// down. Only resolution/fps/codec changes still go through a full
    /// `Pipeline` rebuild (a different, much rarer code path in capture.rs).
    ///
    /// `force_idr` sets `resetEncoder` (see field doc above) — only when a
    /// real keyframe is actually wanted. A pure bitrate nudge with no
    /// recovery need shouldn't force one: NVENC natively supports adjusting
    /// bitrate live with no keyframe at all, and every unnecessary forced
    /// IDR both costs bandwidth (a keyframe access unit is many times a
    /// P-frame's size) and is one more opportunity for whatever's still
    /// unverified about this reconfigure path to go wrong.
    pub unsafe fn reconfigure(&mut self, bitrate_bps: u32, force_idr: bool) -> Result<(), String> {
        let mut config = build_config(
            &self.functions,
            self.encoder,
            self.codec,
            self.fps,
            bitrate_bps,
            self.hdr10,
        )?;

        let mut init_params: InitializeParams = std::mem::zeroed();
        init_params.version = NV_ENC_INITIALIZE_PARAMS_VER;
        init_params.encode_guid = self.codec.encode_guid();
        init_params.preset_guid = NV_ENC_PRESET_P4_GUID;
        init_params.encode_width = self.width;
        init_params.encode_height = self.height;
        init_params.dar_width = self.width;
        init_params.dar_height = self.height;
        init_params.frame_rate_num = self.fps;
        init_params.frame_rate_den = 1;
        init_params.enable_encode_async = 1;
        init_params.enable_ptd = 1;
        init_params.encode_config = &mut config;
        init_params.max_encode_width = self.width;
        init_params.max_encode_height = self.height;
        init_params.tuning_info = NVENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        init_params.buffer_format = if self.hdr10 {
            NVENC_BUFFER_FORMAT_ABGR10
        } else {
            NVENC_BUFFER_FORMAT_ARGB
        };

        let mut reconfigure_params: ReconfigureParams = std::mem::zeroed();
        reconfigure_params.version = NV_ENC_RECONFIGURE_PARAMS_VER;
        reconfigure_params.re_init_encode_params = init_params;
        // resetEncoder = bit 0; forceIDR (bit 1) not needed on top of it since
        // resetEncoder already forces IDR when enable_ptd=1 (see doc comment).
        reconfigure_params.bit_fields = if force_idr { 1 } else { 0 };

        check(
            "NvEncReconfigureEncoder",
            (self.functions.reconfigure_encoder)(self.encoder.as_ptr(), &mut reconfigure_params),
        )
    }

    /// Encode whatever is currently in the registered texture. `force_idr`
    /// requests a keyframe; the caller is responsible for having already
    /// `CopyResource`'d the frame to encode into the texture passed to `new`.
    pub unsafe fn encode(&mut self, force_idr: bool) -> Result<Option<EncodedFrame>, String> {
        let mut map_params: MapInputResource = std::mem::zeroed();
        map_params.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
        map_params.registered_resource = self.registered_resource;
        check(
            "NvEncMapInputResource",
            (self.functions.map_input_resource)(self.encoder.as_ptr(), &mut map_params),
        )?;

        let mut pic_params: PicParams = std::mem::zeroed();
        pic_params.version = NV_ENC_PIC_PARAMS_VER;
        pic_params.input_width = self.width;
        pic_params.input_height = self.height;
        pic_params.input_pitch = self.width;
        pic_params.input_buffer = map_params.mapped_resource;
        pic_params.output_bitstream = self.bitstream_buffer;
        pic_params.completion_event = self.completion_event.0;
        pic_params.buffer_format = if self.hdr10 {
            NVENC_BUFFER_FORMAT_ABGR10
        } else {
            NVENC_BUFFER_FORMAT_ARGB
        };
        pic_params.picture_struct = NVENC_PIC_STRUCT_FRAME;
        pic_params.picture_type = NVENC_PIC_TYPE_AUTOSELECT;
        pic_params.input_time_stamp = self.frame_index as u64;
        if force_idr {
            pic_params.encode_pic_flags |= PIC_FLAG_FORCEIDR;
        }
        self.frame_index += 1;

        let encode_result = (self.functions.encode_picture)(self.encoder.as_ptr(), &mut pic_params);
        // NV_ENC_ERR_NEED_MORE_INPUT (17): normal for the first couple of
        // frames while the encoder's internal pipeline fills — not a real
        // error, just means no bitstream is ready yet.
        if encode_result != 0 && encode_result != 17 {
            let _ = (self.functions.unmap_input_resource)(
                self.encoder.as_ptr(),
                map_params.mapped_resource,
            );
            return Err(err("NvEncEncodePicture", encode_result));
        }

        // Sunshine's own pattern: a bounded wait, not an indefinite one —
        // if the driver doesn't signal completion promptly, drop this frame
        // and move on rather than risk hanging the capture thread forever
        // (exactly the failure mode the Media Foundation path had).
        let wait = WaitForSingleObject(self.completion_event, 100);
        let _ = (self.functions.unmap_input_resource)(
            self.encoder.as_ptr(),
            map_params.mapped_resource,
        );
        if wait != WAIT_OBJECT_0 {
            crate::logging::log("NVENC: completion wait timed out, dropping this frame");
            return Ok(None);
        }
        if encode_result == 17 {
            return Ok(None);
        }

        let mut lock_params: LockBitstream = std::mem::zeroed();
        lock_params.version = NV_ENC_LOCK_BITSTREAM_VER;
        lock_params.output_bit_stream = self.bitstream_buffer;
        lock_params.bit_fields = 1; // do_not_wait — we already waited on the event
        check(
            "NvEncLockBitstream",
            (self.functions.lock_bitstream)(self.encoder.as_ptr(), &mut lock_params),
        )?;
        let bytes = std::slice::from_raw_parts(
            lock_params.bitstream_buffer_ptr as *const u8,
            lock_params.bitstream_size_in_bytes as usize,
        )
        .to_vec();
        check(
            "NvEncUnlockBitstream",
            (self.functions.unlock_bitstream)(self.encoder.as_ptr(), self.bitstream_buffer),
        )?;

        let keyframe = self.codec.is_keyframe(&bytes);

        Ok(Some(EncodedFrame {
            data: bytes,
            keyframe,
            width: self.width,
            height: self.height,
            timestamp: lock_params.output_time_stamp,
        }))
    }

    /// `NvEncInvalidateRefFrames` for each timestamp — the cheap loss
    /// recovery this session's whole recreate-on-IDR machinery exists to
    /// avoid: the driver marks the named reference frames (and, per NVIDIA's
    /// docs, anything reconstructed from them) unusable for motion
    /// estimation, encodes the next frame against an older still-valid
    /// reference, and produces an intra frame itself if none remains — no
    /// keyframe burst, no session teardown. Timestamps are
    /// `EncodedFrame::timestamp` values from *this* session; any driver
    /// error aborts so the caller can fall back to a forced IDR.
    pub unsafe fn invalidate_ref_frames(&mut self, timestamps: &[u64]) -> Result<(), String> {
        for &ts in timestamps {
            check(
                "NvEncInvalidateRefFrames",
                (self.functions.invalidate_ref_frames)(self.encoder.as_ptr(), ts),
            )?;
        }
        Ok(())
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.functions.unregister_resource)(
                self.encoder.as_ptr(),
                self.registered_resource,
            );
            let _ = (self.functions.destroy_bitstream_buffer)(
                self.encoder.as_ptr(),
                self.bitstream_buffer,
            );
            let mut event_params: EventParams = std::mem::zeroed();
            event_params.version = NV_ENC_EVENT_PARAMS_VER;
            event_params.completion_event = self.completion_event.0;
            let _ =
                (self.functions.unregister_async_event)(self.encoder.as_ptr(), &mut event_params);
            let _ = CloseHandle(self.completion_event);
            let _ = (self.functions.destroy_encoder)(self.encoder.as_ptr());
        }
    }
}
