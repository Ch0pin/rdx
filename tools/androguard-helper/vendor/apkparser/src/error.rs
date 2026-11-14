use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Broken APK: {0}")]
    BrokenAPK(String),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Unsupported: {0}")]
    Unsupported(String),

    #[error("File not present: {0}")]
    FileNotPresent(String),

    #[error("Invalid resource: {0}")]
    InvalidResource(String),

    #[error("API level not found: {0}")]
    ApiLevelNotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Error type matching Python's BrokenAPKError.
#[derive(Error, Debug)]
#[error("Broken APK: {0}")]
pub struct BrokenAPKError(pub String);

impl From<BrokenAPKError> for Error {
    fn from(e: BrokenAPKError) -> Self {
        Error::BrokenAPK(e.0)
    }
}
