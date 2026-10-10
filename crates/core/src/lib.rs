//! Client session logic: frame reassembly, stats (IDR policy lands in M1 follow-ups).

pub mod annex_b;
pub mod frame_gate;
pub mod frame_order;
pub mod obu;
pub mod reassemble;
pub mod saved_host;
pub mod session;
pub mod wol;

pub use annex_b::{
    AccessUnit, access_unit_is_keyframe, access_unit_to_avcc_sample, access_unit_without_sei,
    avcc_from_sps_pps, extract_avc_decoder_config, idr_sample_avcc, nal_with_start_code,
    split_access_units, split_nal_units,
};
pub use frame_gate::{Decision, FrameGate};
pub use frame_order::FrameOrder;
pub use obu::{
    Av1SeqInfo, access_unit_is_keyframe as av1_access_unit_is_keyframe,
    access_unit_without_redundant_headers as av1_access_unit_without_redundant_headers,
    av1c_from_sequence_header, extract_sequence_header as av1_extract_sequence_header,
    obu_type_summary as av1_obu_type_summary, parse_sequence_header as parse_av1_sequence_header,
};
pub use reassemble::{FrameReassembler, FrameStats, LossReport};
pub use saved_host::{
    SavedHost, clear as clear_saved_host, load as load_saved_host, save as save_saved_host,
};
pub use session::{
    ClientSession, connect, connect_as, connect_as_identity, connect_known, fallback_video_caps,
    request_idr, request_invalidate_ref_frames, request_power, send_input,
};
pub use wol::send_magic_packets;
