use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    UnexpectedEof {
        offset: usize,
        needed: usize,
        len: usize,
    },
    InvalidMagic([u8; 8]),
    UnsupportedEndian(u32),
    InvalidHeader(&'static str),
    InvalidIndex {
        table: &'static str,
        index: u32,
        len: usize,
    },
    InvalidData {
        offset: usize,
        message: &'static str,
    },
    InvalidUtf8 {
        offset: usize,
    },
}

impl Error {
    pub(crate) fn data(offset: usize, message: &'static str) -> Self {
        Self::InvalidData { offset, message }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::UnexpectedEof {
                offset,
                needed,
                len,
            } => write!(
                formatter,
                "unexpected end of DEX at 0x{offset:x}: need {needed} bytes, file has {len}"
            ),
            Self::InvalidMagic(magic) => write!(formatter, "invalid DEX magic: {magic:?}"),
            Self::UnsupportedEndian(tag) => {
                write!(formatter, "unsupported DEX endian tag 0x{tag:08x}")
            }
            Self::InvalidHeader(message) => write!(formatter, "invalid DEX header: {message}"),
            Self::InvalidIndex { table, index, len } => {
                write!(formatter, "{table} index {index} is outside table length {len}")
            }
            Self::InvalidData { offset, message } => {
                write!(formatter, "invalid DEX data at 0x{offset:x}: {message}")
            }
            Self::InvalidUtf8 { offset } => {
                write!(formatter, "invalid modified UTF-8 string at 0x{offset:x}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
