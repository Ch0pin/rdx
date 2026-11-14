//! APKM (APKMirror split-APK container) support.
//!
//! An `.apkm` file is a ZIP archive that typically contains:
//! - `base.apk` — primary application APK (required for analysis)
//! - `split_config.*.apk` / feature splits — ABI, density, language, dynamic features
//! - `info.json` — APKMirror metadata (optional)
//! - `icon.png`, `APKM_installer.url`, `META-INF/` — packaging extras
//!
//! Analysis tools usually want the **base APK** (manifest + primary DEX). Native
//! libs and density resources often live in splits — callers can enumerate and
//! open those APKs separately via [`ApkmArchive`].

use std::path::Path;

use crate::error::{Error, Result};
use crate::zip::{ZipArchive, ZipIndex};

/// Kind of entry inside an APKM (or similar split container).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApkmEntryKind {
    Base,
    Split,
    Meta,
    Other,
}

/// One file entry discovered in an APKM archive.
#[derive(Debug, Clone)]
pub struct ApkmEntry {
    pub name: String,
    pub kind: ApkmEntryKind,
    pub size: usize,
}

/// Parsed APKM / split-APK container.
pub struct ApkmArchive {
    zip: ZipArchive,
    entries: Vec<ApkmEntry>,
    base_name: String,
    info_json: Option<String>,
}

impl ApkmArchive {
    /// Parse APKM (or APKM-shaped ZIP) bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if !looks_like_apkm(data) {
            return Err(Error::Parse(
                "not an APKM: expected ZIP with base.apk (or sole *.apk) and no root AndroidManifest.xml"
                    .into(),
            ));
        }
        let zip = ZipArchive::parse_slice(data)?;
        let (entries, base_name, info_json) = classify_entries(&zip)?;
        Ok(Self {
            zip,
            entries,
            base_name,
            info_json,
        })
    }

    /// Parse from a filesystem path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)?;
        Self::from_bytes(&data)
    }

    /// Base APK entry name (usually `base.apk`).
    pub fn base_name(&self) -> &str {
        &self.base_name
    }

    /// Raw `info.json` text when present.
    pub fn info_json(&self) -> Option<&str> {
        self.info_json.as_deref()
    }

    /// All classified entries.
    pub fn entries(&self) -> &[ApkmEntry] {
        &self.entries
    }

    /// Split APK entry names (excludes base).
    pub fn split_names(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.kind == ApkmEntryKind::Split)
            .map(|e| e.name.as_str())
            .collect()
    }

    /// Read the base APK bytes.
    pub fn base_apk_bytes(&self) -> Result<Vec<u8>> {
        self.zip.read_to_vec(&self.base_name)
    }

    /// Read any entry by archive path.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.zip.read_to_vec(name)
    }

    /// Extract base + all split `*.apk` files into `out_dir`.
    /// Returns paths written (base first).
    pub fn extract_apks_to(&self, out_dir: &Path) -> Result<Vec<std::path::PathBuf>> {
        std::fs::create_dir_all(out_dir)?;
        let mut written = Vec::new();
        for e in &self.entries {
            if e.kind != ApkmEntryKind::Base && e.kind != ApkmEntryKind::Split {
                continue;
            }
            let dest = out_dir.join(safe_filename(&e.name));
            let bytes = self.zip.read_to_vec(&e.name)?;
            std::fs::write(&dest, bytes)?;
            written.push(dest);
        }
        // Ensure base is first.
        written.sort_by_key(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("base.apk") || name == safe_filename(&self.base_name) {
                0
            } else {
                1
            }
        });
        Ok(written)
    }
}

/// True when `raw` looks like an APKM (or APKM-shaped split ZIP), not a plain APK.
pub fn looks_like_apkm(raw: &[u8]) -> bool {
    if raw.len() < 4 || !raw.starts_with(b"PK") {
        return false;
    }
    let Ok(zip) = ZipIndex::parse(raw) else {
        return false;
    };
    let names: Vec<String> = zip.namelist().map(|s| s.to_string()).collect();
    if zip_has_root_manifest(names.iter().map(|s| s.as_str())) {
        return false;
    }
    find_base_apk_name(&names).is_some()
}

/// If `raw` is an APKM, return base APK bytes; if already an APK, return a copy.
/// Useful one-shot helper for tools that only need a single analyzable APK.
pub fn unwrap_to_apk_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    if looks_like_apkm(raw) {
        return ApkmArchive::from_bytes(raw)?.base_apk_bytes();
    }
    if is_plain_apk(raw) {
        return Ok(raw.to_vec());
    }
    Err(Error::Parse(
        "expected APK or APKM (ZIP with AndroidManifest.xml, or base.apk splits)".into(),
    ))
}

/// True when `raw` is a plain APK ZIP with a root `AndroidManifest.xml`.
pub fn is_plain_apk(raw: &[u8]) -> bool {
    if raw.len() < 4 || !raw.starts_with(b"PK") {
        return false;
    }
    let Ok(zip) = ZipIndex::parse(raw) else {
        return false;
    };
    zip_has_root_manifest(zip.namelist())
}

/// Write a minimal synthetic APKM for tests (base + one split + info.json).
#[cfg(test)]
pub fn write_test_apkm(base_apk: &[u8], split_apk: &[u8]) -> Result<Vec<u8>> {
    use std::io::{Cursor, Write};
    let cursor = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    let opts =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("info.json", opts)
        .map_err(|e| Error::Parse(e.to_string()))?;
    zip.write_all(br#"{"apkm_version":1,"pname":"com.example.test"}"#)
        .map_err(Error::Io)?;
    zip.start_file("base.apk", opts)
        .map_err(|e| Error::Parse(e.to_string()))?;
    zip.write_all(base_apk).map_err(Error::Io)?;
    zip.start_file("split_config.arm64_v8a.apk", opts)
        .map_err(|e| Error::Parse(e.to_string()))?;
    zip.write_all(split_apk).map_err(Error::Io)?;
    let cursor = zip.finish().map_err(|e| Error::Parse(e.to_string()))?;
    Ok(cursor.into_inner())
}

fn classify_entries(zip: &ZipArchive) -> Result<(Vec<ApkmEntry>, String, Option<String>)> {
    let names = zip.names();
    let base_name = find_base_apk_name(&names)
        .ok_or_else(|| Error::Parse("APKM missing base.apk (or any *.apk)".into()))?
        .to_string();

    let mut entries = Vec::new();
    let mut info_json = None;
    for name in &names {
        let lower = name.to_ascii_lowercase();
        let size = zip.uncompressed_size(name).unwrap_or(0) as usize;
        let kind = if name == &base_name || lower.ends_with("/base.apk") {
            ApkmEntryKind::Base
        } else if is_apk_entry(name) {
            ApkmEntryKind::Split
        } else if lower.ends_with("info.json")
            || lower.ends_with("icon.png")
            || lower.contains("apkm_installer")
            || lower.starts_with("meta-inf/")
        {
            ApkmEntryKind::Meta
        } else {
            ApkmEntryKind::Other
        };
        if lower == "info.json" || lower.ends_with("/info.json") {
            if let Ok(bytes) = zip.read(name) {
                info_json = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        entries.push(ApkmEntry {
            name: name.clone(),
            kind,
            size,
        });
    }
    Ok((entries, base_name, info_json))
}

fn find_base_apk_name(names: &[String]) -> Option<&str> {
    // Prefer explicit base.apk (any path depth).
    if let Some(n) = names.iter().find(|n| {
        let file = n.rsplit('/').next().unwrap_or(n);
        file.eq_ignore_ascii_case("base.apk")
    }) {
        return Some(n.as_str());
    }
    // Fallback: single *.apk in the archive.
    let apks: Vec<&str> = names
        .iter()
        .filter(|n| is_apk_entry(n))
        .map(|s| s.as_str())
        .collect();
    if apks.len() == 1 {
        return Some(apks[0]);
    }
    // Prefer non-split_config name when multiple.
    apks.into_iter().find(|n| {
        let file = n.rsplit('/').next().unwrap_or(n).to_ascii_lowercase();
        !file.starts_with("split_config.") && !file.starts_with("config.")
    })
}

fn is_apk_entry(name: &str) -> bool {
    let file = name.rsplit('/').next().unwrap_or(name);
    file.to_ascii_lowercase().ends_with(".apk") && !file.starts_with('.')
}

fn zip_has_root_manifest<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
    names.any(|n| {
        n == "AndroidManifest.xml"
            || (n.ends_with("/AndroidManifest.xml")
                && !n.to_ascii_lowercase().contains(".apk/")
                && n.matches('/').count() <= 1)
    })
}

fn safe_filename(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .replace(['\\', '/', '\0'], "_");
    if base.is_empty() || base == "." || base == ".." {
        "_entry".into()
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn minimal_apk_zip(label: &[u8]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("AndroidManifest.xml", opts).unwrap();
        zip.write_all(label).unwrap();
        zip.start_file("classes.dex", opts).unwrap();
        zip.write_all(b"dex\n000").unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn detect_and_unwrap_apkm() {
        let base = minimal_apk_zip(b"BASE");
        let split = minimal_apk_zip(b"SPLIT");
        let apkm = write_test_apkm(&base, &split).unwrap();
        assert!(looks_like_apkm(&apkm));
        assert_eq!(crate::utils::is_android_raw(&apkm), Some("APKM"));
        assert!(!looks_like_apkm(&base));
        assert!(is_plain_apk(&base));
        assert_eq!(crate::utils::is_android_raw(&base), Some("APK"));

        let arch = ApkmArchive::from_bytes(&apkm).unwrap();
        assert_eq!(arch.base_name(), "base.apk");
        assert_eq!(arch.split_names(), vec!["split_config.arm64_v8a.apk"]);
        assert!(arch.info_json().unwrap().contains("com.example.test"));
        let unwrapped = unwrap_to_apk_bytes(&apkm).unwrap();
        assert_eq!(unwrapped, base);
        assert_eq!(unwrap_to_apk_bytes(&base).unwrap(), base);
    }

    #[test]
    fn extract_apks_to_dir() {
        let base = minimal_apk_zip(b"BASE");
        let split = minimal_apk_zip(b"SPLIT");
        let apkm = write_test_apkm(&base, &split).unwrap();
        let dir = tempfile_dir();
        let paths = ApkmArchive::from_bytes(&apkm)
            .unwrap()
            .extract_apks_to(&dir)
            .unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("base.apk"));
        assert_eq!(std::fs::read(&paths[0]).unwrap(), base);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("apkparser_apkm_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
