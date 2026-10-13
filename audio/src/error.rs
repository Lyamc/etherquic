use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ogg: {0}")]
    Ogg(String),
    #[error("opus: {0}")]
    Opus(String),
    #[error("virtual sound: {0}")]
    Virtual(String),
    #[error(transparent)]
    Loc(#[from] etherquic_container::Error),
}
