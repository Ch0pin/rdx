//! Error types for DEX parsing.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DexError {
    #[error("DEX parse error: {0}")]
    Parse(String),

    #[error("Invalid magic or unsupported DEX version")]
    InvalidMagic,

    #[error("Truncated or out of bounds: {0}")]
    Truncated(String),

    #[error("slice unsupported ref (invoke-custom / method-handle)")]
    UnsupportedRef,
}

pub type Result<T> = std::result::Result<T, DexError>;
