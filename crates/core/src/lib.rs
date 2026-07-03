//! Client session logic: frame reassembly, stats (IDR policy lands in M1 follow-ups).

pub mod annex_b;
pub mod reassemble;
pub mod session;

pub use annex_b::{split_access_units, split_nal_units, AccessUnit};
pub use reassemble::{FrameReassembler, FrameStats};
pub use session::{connect, connect_as, ClientSession};
