//! Early-exit `--getclass` path: locate class → slice → decompile without full APK load.

use apkparser::{dex_entries, iter_logical_dexes, ZipIndex};
use dex_parser::{dex_defines_class, DexSlicer};

use crate::error::{DexDecompilerError, Result};
use crate::{parse_dex, Decompiler, DecompilerOptions, DexFile};

/// Normalize a user class name to a Dalvik descriptor (`Lcom/foo/Bar;`).
pub fn to_dalvik_descriptor(class: &str) -> String {
    let n = class.trim();
    if n.starts_with('L') && n.ends_with(';') {
        return n.to_string();
    }
    format!("L{};", n.replace('.', "/"))
}

fn looks_like_dex(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..4] == b"dex\n"
}

fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0..2] == [0x50, 0x4b]
}

/// Locate `descriptor` in DEX or APK bytes, returning the owning DEX bytes (inflated).
///
/// Never uses [`crate::input::load_dexes_from_path`]. For APKs, inflates `classes*.dex`
/// smallest-first and cancels once a defining DEX is found.
pub fn find_dex_defining_class(bytes: &[u8], descriptor: &str) -> Result<Vec<u8>> {
    if looks_like_dex(bytes) {
        if dex_defines_class(bytes, descriptor) {
            return Ok(bytes.to_vec());
        }
        return Err(DexDecompilerError::Parse(format!(
            "class {descriptor} not defined in DEX"
        )));
    }
    if !looks_like_zip(bytes) {
        return Err(DexDecompilerError::Parse(
            "input is neither DEX nor ZIP/APK".into(),
        ));
    }
    let index = ZipIndex::parse(bytes)
        .map_err(|e| DexDecompilerError::Parse(format!("ZipIndex: {e}")))?;
    let entries = dex_entries(&index);
    if entries.is_empty() {
        return Err(DexDecompilerError::Parse(
            "APK/ZIP contains no classes*.dex".into(),
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let found = entries.par_iter().find_map_any(|e| {
            let data = index.read_entry(e).ok()?;
            for (_name, dex_bytes) in iter_logical_dexes(&e.filename, data) {
                if dex_defines_class(&dex_bytes, descriptor) {
                    return Some(dex_bytes);
                }
            }
            None
        });
        if let Some(b) = found {
            return Ok(b);
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        for e in &entries {
            let data = index
                .read_entry(e)
                .map_err(|err| DexDecompilerError::Parse(format!("inflate {}: {err}", e.filename)))?;
            for (_name, dex_bytes) in iter_logical_dexes(&e.filename, data) {
                if dex_defines_class(&dex_bytes, descriptor) {
                    return Ok(dex_bytes);
                }
            }
        }
    }

    // Native path may have skipped sequential fallback when par_iter found nothing.
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Already searched in parallel; nothing found.
    }

    Err(DexDecompilerError::Parse(format!(
        "class {descriptor} not found in any DEX"
    )))
}

/// Slice a minimal DEX containing only `class_name` from APK/DEX bytes.
pub fn slice_class_from_input(bytes: &[u8], class_name: &str) -> Result<Vec<u8>> {
    let descriptor = to_dalvik_descriptor(class_name);
    let dex_bytes = find_dex_defining_class(bytes, &descriptor)?;
    DexSlicer::new(&dex_bytes)
        .and_then(|s| s.slice_class(&descriptor))
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))
}

/// Decompile a single class via the getclass early-exit path.
pub fn getclass_java(bytes: &[u8], class_name: &str, options: DecompilerOptions) -> Result<String> {
    let sliced = slice_class_from_input(bytes, class_name)?;
    let dex = parse_dex(&sliced)?;
    decompile_only_class(&dex, class_name, options)
}

fn decompile_only_class(
    dex: &DexFile,
    class_name: &str,
    options: DecompilerOptions,
) -> Result<String> {
    let want = to_dalvik_descriptor(class_name);
    let d = Decompiler::with_options(dex, options);
    for r in dex.class_defs() {
        let cd = r.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let ty = dex
            .get_type(cd.class_idx)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        if ty == want {
            return d.decompile_class(&cd);
        }
    }
    Err(DexDecompilerError::Parse(format!(
        "class {class_name} missing from sliced DEX"
    )))
}
