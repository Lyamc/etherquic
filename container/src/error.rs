use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("need more bytes")]
    NeedMore,
    #[error("truncated LOC object")]
    Truncated,
    #[error("non-minimal QUIC varint")]
    NonMinimalVarint,
    #[error("varint exceeds 62 bits")]
    VarintOverflow,
    #[error("property block length does not match its properties")]
    BadPropertyBlock,
    #[error("property {id:#x} has the wrong value shape")]
    BadProperty { id: u64 },
    #[error("video frame marking must be 1 to 4 bytes")]
    BadFrameMark,
    #[error("{0}")]
    Message(String),
}
