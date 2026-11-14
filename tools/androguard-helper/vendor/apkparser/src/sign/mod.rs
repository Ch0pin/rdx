//! APK signing (v1 JAR + v2/v3 APK Signing Block).

mod debug_keystore;
mod v1_write;
mod v2_write;
mod zipalign;

use sha2::{Digest, Sha256};

pub use debug_keystore::{
    deserialize_debug_keystore, generate_debug_keystore_ephemeral, serialize_debug_keystore,
};
pub use v1_write::sign_v1;
pub use v2_write::{sign_v2, sign_v2_v3};
pub use zipalign::zipalign;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SignError {
    #[error("invalid APK: {0}")]
    InvalidApk(String),
    #[error("signing error: {0}")]
    Signing(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type SignResult<T> = std::result::Result<T, SignError>;

/// Which APK signature schemes to apply.
#[derive(Clone, Debug)]
pub struct SignOptions {
    pub v1: bool,
    pub v2: bool,
    pub v3: bool,
    pub keystore: KeystoreMaterial,
}

impl Default for SignOptions {
    fn default() -> Self {
        Self {
            v1: false,
            v2: true,
            v3: true,
            keystore: KeystoreMaterial::debug(),
        }
    }
}

/// Signing key material.
#[derive(Clone, Debug)]
pub struct KeystoreMaterial {
    pub private_key_der: Vec<u8>,
    pub certificate_der: Vec<u8>,
    pub is_ec: bool,
}

impl KeystoreMaterial {
    /// Generate a debug signing key (RSA-2048, self-signed).
    ///
    /// On native targets this caches under `~/.cache/apk-patch/`.
    pub fn debug() -> Self {
        debug_keystore::generate_debug_keystore().expect("debug keystore generation")
    }

    /// Generate a debug key without touching the filesystem (WASM-safe).
    pub fn debug_ephemeral() -> Self {
        debug_keystore::generate_debug_keystore_ephemeral().expect("debug keystore generation")
    }
}

/// Align then sign an APK.
pub fn align_and_sign(apk: &[u8], options: &SignOptions) -> SignResult<Vec<u8>> {
    let aligned = zipalign(apk, 4)?;
    sign_apk(&aligned, options)
}

/// Sign an APK (expects 4-byte aligned input for v2/v3).
pub fn sign_apk(apk: &[u8], options: &SignOptions) -> SignResult<Vec<u8>> {
    let mut signed = apk.to_vec();

    if options.v1 {
        signed = sign_v1(&signed, &options.keystore)?;
    }

    if options.v2 || options.v3 {
        signed = sign_v2_v3(&signed, &options.keystore, options.v2, options.v3)?;
    }

    Ok(signed)
}

pub(crate) fn central_dir_offset(data: &[u8]) -> SignResult<usize> {
    let eocd = find_eocd(data).ok_or_else(|| SignError::InvalidApk("EOCD not found".into()))?;
    Ok(u32::from_le_bytes([
        data[eocd + 16],
        data[eocd + 17],
        data[eocd + 18],
        data[eocd + 19],
    ]) as usize)
}

pub(crate) fn jar_manifest_digest(data: &[u8]) -> String {
    let digest = sha256(data);
    format!("SHA-256-Digest: {}\r\n", base64_encode(&digest))
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub(crate) fn read_zip_entries(apk: &[u8]) -> SignResult<std::collections::BTreeMap<String, Vec<u8>>> {
    use std::collections::BTreeMap;
    use std::io::Cursor;
    let cursor = Cursor::new(apk);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| SignError::InvalidApk(format!("zip: {e}")))?;
    let mut map = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| SignError::InvalidApk(format!("zip index: {e}")))?;
        let name = file.name().to_string();
        let mut data = Vec::new();
        std::io::copy(&mut file, &mut data)?;
        map.insert(name, data);
    }
    Ok(map)
}

pub(crate) fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let mut pos = data.len().saturating_sub(22);
    while pos > 0 {
        if data.get(pos..pos + 4)? == [0x50, 0x4b, 0x05, 0x06] {
            let comment_len = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
            if pos + 22 + comment_len == data.len() {
                return Some(pos);
            }
        }
        pos -= 1;
    }
    None
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

