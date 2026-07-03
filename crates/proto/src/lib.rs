//! Pure wire types and framing for the StreamBeam protocol.
//!
//! No I/O in this crate: everything here is deterministic bytes-in/bytes-out,
//! so it builds and tests on any platform. The wire format is documented in
//! `docs/PROTOCOL.md`, which must be kept in lockstep with this crate.

pub mod control;
pub mod fec;
pub mod frame;
pub mod input;
pub mod media;
pub mod pair;

pub const PROTO_VERSION: u8 = 1;

/// ALPN identifier for the QUIC connection.
pub const ALPN: &[u8] = b"streambeam/1";

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("buffer too short: need {need} bytes, got {got}")]
    TooShort { need: usize, got: usize },
    #[error("invalid value for {field}: {value}")]
    Invalid { field: &'static str, value: u64 },
    #[error("frame exceeds {max} shards (frame_len {frame_len}, shard_len {shard_len})")]
    FrameTooLarge {
        max: usize,
        frame_len: usize,
        shard_len: usize,
    },
    #[error("not enough shards to recover frame: have {have}, need {need}")]
    NotEnoughShards { have: usize, need: usize },
    #[error("reed-solomon: {0}")]
    ReedSolomon(#[from] reed_solomon_simd::Error),
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("bad pairing payload: {0}")]
    BadPairPayload(&'static str),
}
