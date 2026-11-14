//! Utilities: is_android_raw, read_uint32_le (mirrors apkparser.utils).

use std::io::Read;

use crate::error::Result;

/// Read a little-endian u32 from a reader.
pub fn read_uint32_le<R: Read>(reader: &mut R) -> Result<u32> {
    use byteorder::{LittleEndian, ReadBytesExt};
    reader
        .read_u32::<LittleEndian>()
        .map_err(|e| crate::error::Error::Io(e))
}

/// Returns the type of file for common Android formats (APK, APKM, DEX, DEY, AXML, ARSC).
/// Mirrors Python `is_android_raw`, extended with APKM (APKMirror split containers).
pub fn is_android_raw(raw: &[u8]) -> Option<&'static str> {
    if raw.len() < 4 {
        return None;
    }
    if raw.starts_with(b"PK") {
        // Prefer ZIP entry inspection so nested `AndroidManifest.xml` bytes inside
        // `base.apk` do not mis-classify an APKM as a plain APK.
        if crate::apkm::looks_like_apkm(raw) {
            return Some("APKM");
        }
        if crate::apkm::is_plain_apk(raw) {
            return Some("APK");
        }
        // Fast fallback for truncated / odd ZIPs that still embed the manifest name.
        if raw.windows(18).any(|w| w == b"AndroidManifest.xml") {
            return Some("APK");
        }
    }
    if raw.starts_with(b"dex") {
        return Some("DEX");
    }
    if raw.starts_with(b"dey") {
        return Some("DEY");
    }
    if raw[0..4] == [0x03, 0x00, 0x08, 0x00] || raw[0..4] == [0x00, 0x00, 0x08, 0x00] {
        return Some("AXML");
    }
    if raw[0..4] == [0x02, 0x00, 0x0C, 0x00] {
        return Some("ARSC");
    }
    None
}

/// Return the type of file (APK, DEX, etc.) by reading from path.
pub fn is_android(path: &std::path::Path) -> Result<Option<&'static str>> {
    let raw = std::fs::read(path).map_err(crate::error::Error::Io)?;
    Ok(is_android_raw(&raw))
}
