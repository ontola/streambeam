use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("rcgen: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("quinn: {0}")]
    Quinn(#[from] quinn::ConnectError),
    #[error("quinn config: {0}")]
    QuinnConfig(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("proto: {0}")]
    Proto(#[from] streambeam_proto::ProtoError),
    #[error("pairing rejected: {0}")]
    PairRejected(String),
    #[error("invalid certificate: {0}")]
    InvalidCert(&'static str),
    #[error("missing peer certificate")]
    MissingPeerCert,
    #[error("stream closed")]
    StreamClosed,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, NetError>;
