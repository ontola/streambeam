//! Client session logic: frame reassembly, stats (IDR policy lands in M1 follow-ups).

pub mod annex_b;
pub mod reassemble;
pub mod saved_host;
pub mod session;

pub use annex_b::{
    AccessUnit, access_unit_is_keyframe, access_unit_to_avcc_sample, access_unit_without_sei,
    avcc_from_sps_pps, extract_avc_decoder_config, idr_sample_avcc, nal_with_start_code,
    split_access_units, split_nal_units,
};
pub use reassemble::{FrameReassembler, FrameStats, LossReport};
pub use saved_host::{
    SavedHost, clear as clear_saved_host, load as load_saved_host, save as save_saved_host,
};
pub use session::{
    ClientSession, connect, connect_as, connect_as_identity, connect_known, request_idr,
    request_invalidate_ref_frames, send_input,
};
