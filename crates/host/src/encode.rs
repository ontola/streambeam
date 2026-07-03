//! Video encoder abstraction — hwcodec / MF / NVENC impls plug in behind this (M2).

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder not ready")]
    NotReady,
    #[error("{0}")]
    Other(String),
}

/// Host-side video encoder (WGC texture in → Annex-B access units out).
pub trait VideoEncoder: Send {
    fn encode_next(&mut self) -> Result<Option<EncodedFrame>, EncodeError>;
    fn force_idr(&mut self);
    fn dimensions(&self) -> (u32, u32);
}
