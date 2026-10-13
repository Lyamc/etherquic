use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("input: {0}")]
    Input(String),
    #[error("script line {line}: {message}")]
    Script { line: usize, message: String },
}
