//! Load DEX bytes from `.dex` files or APK/ZIP archives.
//!
//! Path loads prefer memory-mapping on native targets (zero-copy APK view).
//! WASM / fallback builds always use owned `Vec<u8>` (`MappedInput::Bytes`).

use std::io::Cursor;
use std::path::Path;

use apkparser::{dex_entries, iter_logical_dexes, ZipIndex};
use zip::ZipArchive;

use crate::error::{DexDecompilerError, Result};
use crate::{parse_dex, DexFile};

/// Owned or mmapped APK/DEX bytes.
///
/// Use [`MappedInput::open`] for path-based queries. On `wasm32`, only the
/// bytes variant is available so the same API works in browser builds.
pub enum MappedInput {
    /// Memory-mapped file (native only).
    #[cfg(not(target_arch = "wasm32"))]
    Mmap(memmap2::Mmap),
    /// Fully buffered copy (WASM default, or mmap unavailable).
    Bytes(Vec<u8>),
}

impl MappedInput {
    /// Open `path` via mmap when available; otherwise read into memory.
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::fs::File;
            let file = File::open(path).map_err(|e| {
                DexDecompilerError::Parse(format!("open {}: {e}", path.display()))
            })?;
            // Empty files cannot be mmapped on some platforms.
            let meta = file.metadata().map_err(|e| {
                DexDecompilerError::Parse(format!("stat {}: {e}", path.display()))
            })?;
            if meta.len() > 0 {
                // SAFETY: file is opened read-only for the lifetime of the map.
                match unsafe { memmap2::Mmap::map(&file) } {
                    Ok(m) => return Ok(Self::Mmap(m)),
                    Err(_) => { /* fall through to read */ }
                }
            }
        }
        let bytes = std::fs::read(path).map_err(|e| {
            DexDecompilerError::Parse(format!("read {}: {e}", path.display()))
        })?;
        Ok(Self::Bytes(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(m) => m,
            Self::Bytes(b) => b,
        }
    }

    pub fn is_mmap(&self) -> bool {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(_) => true,
            Self::Bytes(_) => false,
        }
    }
}

/// True if `name` looks like a Dalvik `classes*.dex` entry.
pub fn is_classes_dex_name(name: &str) -> bool {
    let name = name.trim_start_matches('/');
    name == "classes.dex" || (name.starts_with("classes") && name.ends_with(".dex"))
}

fn dex_sort_key(name: &str) -> (u32, String) {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base == "classes.dex" {
        return (1, base.to_string());
    }
    if let Some(rest) = base
        .strip_prefix("classes")
        .and_then(|s| s.strip_suffix(".dex"))
    {
        if let Ok(n) = rest.parse::<u32>() {
            return (n, base.to_string());
        }
    }
    (u32::MAX, base.to_string())
}

/// Extract `(entry_name, bytes)` for every `classes*.dex` inside an APK/ZIP.
pub fn extract_dex_entries_from_apk(apk_bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let cursor = Cursor::new(apk_bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| DexDecompilerError::Parse(format!("open APK/ZIP: {e}")))?;
    let mut names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| DexDecompilerError::Parse(format!("zip entry: {e}")))?;
        let name = file.name().to_string();
        if is_classes_dex_name(&name) {
            names.push(name);
        }
    }
    names.sort_by_key(|n| dex_sort_key(n));

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let mut file = archive
            .by_name(&name)
            .map_err(|e| DexDecompilerError::Parse(format!("zip get {name}: {e}")))?;
        let mut buf = Vec::new();
        std::io::copy(&mut file, &mut buf)
            .map_err(|e| DexDecompilerError::Parse(format!("zip read {name}: {e}")))?;
        out.push((name, buf));
    }
    if out.is_empty() {
        return Err(DexDecompilerError::Parse(
            "APK/ZIP contains no classes*.dex".into(),
        ));
    }
    Ok(out)
}

/// Lazy APK DEX extract via apk-parser `ZipIndex` (inflate only `classes*.dex`, smallest-first).
pub fn extract_dex_entries_lazy(apk_bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let index = ZipIndex::parse(apk_bytes)
        .map_err(|e| DexDecompilerError::Parse(format!("open APK ZipIndex: {e}")))?;
    let entries = dex_entries(&index);
    if entries.is_empty() {
        return Err(DexDecompilerError::Parse(
            "APK/ZIP contains no classes*.dex".into(),
        ));
    }
    let mut out = Vec::new();
    for e in entries {
        let fname = e.filename.clone();
        let data = index
            .read_entry(e)
            .map_err(|err| DexDecompilerError::Parse(format!("inflate {fname}: {err}")))?;
        for (name, bytes) in iter_logical_dexes(&fname, data) {
            out.push((name, bytes));
        }
    }
    Ok(out)
}

/// ASC-style findrefs: parallel inflate+scan per `classes*.dex` (native), sequential on WASM.
///
/// Avoids building a full [`DexFile`] per entry — uses RawDex/FastRef then cheap method_ids
/// name resolution. Returns `(dex_index, sites)` in entry order (smallest-compressed first).
pub fn findrefs_on_apk_bytes(
    apk_bytes: &[u8],
    kind: &str,
    value: &str,
    class_desc: Option<&str>,
    exact_class: bool,
) -> Result<Vec<(usize, crate::xref::FastRefSite)>> {
    use crate::xref::{findrefs_kind_on_bytes, FastRefSite};

    if looks_like_dex(apk_bytes) {
        let sites = findrefs_kind_on_bytes(apk_bytes, kind, value, class_desc, exact_class)?;
        return Ok(sites
            .into_iter()
            .map(|s| (0usize, s))
            .collect());
    }

    let index = ZipIndex::parse(apk_bytes)
        .map_err(|e| DexDecompilerError::Parse(format!("open APK ZipIndex: {e}")))?;
    let entries: Vec<_> = dex_entries(&index).into_iter().cloned().collect();
    if entries.is_empty() {
        return Err(DexDecompilerError::Parse(
            "APK/ZIP contains no classes*.dex".into(),
        ));
    }

    // Inflate + FastRef per entry. Parallel on native (ASC process-pool analogue).
    let per_entry: Result<Vec<Vec<(usize, FastRefSite)>>> = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            entries
                .par_iter()
                .enumerate()
                .map(|(dex_i, e)| {
                    let data = index.read_entry(e).map_err(|err| {
                        DexDecompilerError::Parse(format!("inflate {}: {err}", e.filename))
                    })?;
                    let mut sites = Vec::new();
                    for (_name, bytes) in iter_logical_dexes(&e.filename, data) {
                        let hit = findrefs_kind_on_bytes(
                            &bytes,
                            kind,
                            value,
                            class_desc,
                            exact_class,
                        )?;
                        sites.extend(hit.into_iter().map(|s| (dex_i, s)));
                    }
                    Ok(sites)
                })
                .collect()
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut all = Vec::with_capacity(entries.len());
            for (dex_i, e) in entries.iter().enumerate() {
                let data = index.read_entry(e).map_err(|err| {
                    DexDecompilerError::Parse(format!("inflate {}: {err}", e.filename))
                })?;
                let mut sites = Vec::new();
                for (_name, bytes) in iter_logical_dexes(&e.filename, data) {
                    let hit =
                        findrefs_kind_on_bytes(&bytes, kind, value, class_desc, exact_class)?;
                    sites.extend(hit.into_iter().map(|s| (dex_i, s)));
                }
                all.push(sites);
            }
            Ok(all)
        }
    };

    let mut out = Vec::new();
    for chunk in per_entry? {
        out.extend(chunk);
    }
    Ok(out)
}

/// Path-based findrefs: mmap APK on native, then [`findrefs_on_apk_bytes`].
pub fn findrefs_on_apk_path(
    path: &Path,
    kind: &str,
    value: &str,
    class_desc: Option<&str>,
    exact_class: bool,
) -> Result<Vec<(usize, crate::xref::FastRefSite)>> {
    let mapped = MappedInput::open(path)?;
    findrefs_on_apk_bytes(mapped.as_bytes(), kind, value, class_desc, exact_class)
}

/// Extract `AndroidManifest.xml` bytes from an APK/ZIP (binary AXML or text).
pub fn extract_android_manifest_from_apk(apk_bytes: &[u8]) -> Result<Vec<u8>> {
    let cursor = Cursor::new(apk_bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| DexDecompilerError::Parse(format!("open APK/ZIP: {e}")))?;
    let mut file = archive.by_name("AndroidManifest.xml").map_err(|e| {
        DexDecompilerError::Parse(format!("AndroidManifest.xml not found in APK: {e}"))
    })?;
    let mut buf = Vec::new();
    std::io::copy(&mut file, &mut buf)
        .map_err(|e| DexDecompilerError::Parse(format!("read AndroidManifest.xml: {e}")))?;
    Ok(buf)
}

/// True if bytes look like UTF-8/ASCII text XML (decoded manifest), not binary AXML.
pub fn looks_like_text_xml(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(256)];
    if sample.starts_with(&[0x03, 0x00, 0x08, 0x00]) {
        return false;
    }
    let Ok(s) = std::str::from_utf8(sample) else {
        return false;
    };
    let t = s.trim_start();
    t.starts_with("<?xml") || t.starts_with("<manifest") || t.starts_with("<")
}

/// Load one path as DEX or APK (auto-detect by magic / extension).
///
/// Prefers mmap on native targets; WASM always buffers (`MappedInput::Bytes`).
pub fn load_dexes_from_path(path: &Path) -> Result<Vec<DexFile>> {
    let mapped = MappedInput::open(path)?;
    load_dexes_from_bytes(mapped.as_bytes(), path)
}

/// Like [`load_dexes_from_path`], but returns the mapped/buffered input so
/// callers can keep the backing store alive while holding `DexFile` views... 
/// actually DexFile owns its data — this returns `(MappedInput, Vec<DexFile>)`
/// for APIs that still need the raw APK bytes (getclass / manifest).
pub fn load_mapped_apk(path: &Path) -> Result<(MappedInput, Vec<DexFile>)> {
    let mapped = MappedInput::open(path)?;
    let dexes = load_dexes_from_bytes(mapped.as_bytes(), path)?;
    Ok((mapped, dexes))
}

/// Parse raw bytes as a single DEX, or as an APK/ZIP containing DEX files.
pub fn load_dexes_from_bytes(bytes: &[u8], hint_path: &Path) -> Result<Vec<DexFile>> {
    if looks_like_dex(bytes) {
        return Ok(vec![parse_dex(bytes)?]);
    }
    if looks_like_zip(bytes)
        || hint_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "apk" | "zip" | "jar" | "aar"
                )
            })
    {
        let entries = extract_dex_entries_lazy(bytes).or_else(|_| extract_dex_entries_from_apk(bytes))?;
        let mut dexes = Vec::with_capacity(entries.len());
        for (name, data) in entries {
            let dex = parse_dex(&data)
                .map_err(|e| DexDecompilerError::Parse(format!("parse DEX entry {name}: {e}")))?;
            dexes.push(dex);
        }
        return Ok(dexes);
    }
    // Last resort: try DEX parse for better error message.
    Ok(vec![parse_dex(bytes)?])
}

/// Load and concatenate DEX files from one or more paths (DEX and/or APK).
pub fn load_dexes_from_paths(paths: &[impl AsRef<Path>]) -> Result<Vec<DexFile>> {
    let mut all = Vec::new();
    for p in paths {
        all.extend(load_dexes_from_path(p.as_ref())?);
    }
    if all.is_empty() {
        return Err(DexDecompilerError::Parse("no DEX inputs".into()));
    }
    Ok(all)
}

fn looks_like_dex(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..4] == b"dex\n"
}

fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && (bytes[0..2] == [0x50, 0x4b]) // PK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_dex_name_filter() {
        assert!(is_classes_dex_name("classes.dex"));
        assert!(is_classes_dex_name("classes2.dex"));
        assert!(is_classes_dex_name("classes15.dex"));
        assert!(!is_classes_dex_name("AndroidManifest.xml"));
        assert!(!is_classes_dex_name("lib/arm64/libfoo.so"));
    }

    #[test]
    fn text_xml_detection() {
        assert!(looks_like_text_xml(b"<?xml version=\"1.0\"?><manifest/>"));
        assert!(looks_like_text_xml(
            b"<manifest package=\"a.b\"></manifest>"
        ));
        assert!(!looks_like_text_xml(&[0x03, 0x00, 0x08, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn sort_key_orders_multidex() {
        let mut names: Vec<String> = vec![
            "classes3.dex".into(),
            "classes.dex".into(),
            "classes2.dex".into(),
        ];
        names.sort_by_key(|n| dex_sort_key(n));
        assert_eq!(
            names,
            vec![
                "classes.dex".to_string(),
                "classes2.dex".to_string(),
                "classes3.dex".to_string()
            ]
        );
    }

    #[test]
    fn mapped_input_bytes_variant_always_available() {
        let owned = MappedInput::Bytes(b"dex\n035\0".to_vec());
        assert!(!owned.is_mmap());
        assert_eq!(owned.as_bytes(), b"dex\n035\0");
    }

    #[test]
    fn mapped_input_open_empty_falls_back_to_bytes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "dex_decompiler_mmap_empty_{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, b"").unwrap();
        let mapped = MappedInput::open(&path).unwrap();
        assert!(!mapped.is_mmap());
        assert!(mapped.as_bytes().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
