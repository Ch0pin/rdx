//! Android manifest parsing (binary AXML) - mirrors AXMLPrinter usage.

use crate::error::{Error, Result};

/// Parsed Android manifest (package, permissions, min/target SDK, etc.).
#[derive(Debug, Default, Clone)]
pub struct AndroidManifest {
    pub package: Option<String>,
    pub permissions: Vec<String>,
    pub uses_permissions: Vec<String>,
    pub min_sdk_version: Option<u32>,
    pub target_sdk_version: Option<u32>,
    pub version_code: Option<u32>,
    pub version_name: Option<String>,
}

/// Parse binary AndroidManifest.xml bytes (AXML format) using axmldecoder.
///
/// Requires the `manifest-axmldecoder` feature.
///
/// `axmldecoder` 0.2 asserts that element namespaces are `None`; some real APKs
/// (e.g. InsecureBankv2) put the Android URI on the element and panic. Catch that
/// so callers can fall back to `axml-parser` / raw bytes.
#[cfg(feature = "manifest-axmldecoder")]
pub fn parse_manifest(axml_bytes: &[u8]) -> Result<AndroidManifest> {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    match catch_unwind(AssertUnwindSafe(|| parse_manifest_inner(axml_bytes))) {
        Ok(result) => result,
        Err(_) => Err(Error::Parse(
            "AXML: axmldecoder panicked (unsupported namespace on element)".into(),
        )),
    }
}

#[cfg(not(feature = "manifest-axmldecoder"))]
pub fn parse_manifest(_axml_bytes: &[u8]) -> Result<AndroidManifest> {
    Err(Error::Unsupported(
        "manifest parsing requires the `manifest-axmldecoder` feature".into(),
    ))
}

#[cfg(feature = "manifest-axmldecoder")]
fn parse_manifest_inner(axml_bytes: &[u8]) -> Result<AndroidManifest> {
    use std::io::Cursor;

    let mut cursor = Cursor::new(axml_bytes);
    let doc = axmldecoder::parse(&mut cursor).map_err(|e| Error::Parse(format!("AXML: {}", e)))?;
    let mut manifest = AndroidManifest::default();
    let root = match doc.get_root() {
        Some(axmldecoder::Node::Element(e)) => e,
        _ => return Ok(manifest),
    };
    fn collect_manifest(elem: &axmldecoder::Element, manifest: &mut AndroidManifest) {
        let tag = elem.get_tag();
        for (k, v) in elem.get_attributes() {
            match (tag, k.as_str()) {
                ("manifest", "package") => manifest.package = Some(v.clone()),
                ("manifest", "versionCode" | "android:versionCode") => {
                    manifest.version_code = v.parse().ok();
                }
                ("manifest", "versionName" | "android:versionName") => {
                    manifest.version_name = Some(v.clone());
                }
                ("uses-sdk", "minSdkVersion" | "android:minSdkVersion") => {
                    manifest.min_sdk_version = v.parse().ok();
                }
                ("uses-sdk", "targetSdkVersion" | "android:targetSdkVersion") => {
                    manifest.target_sdk_version = v.parse().ok();
                }
                ("uses-permission" | "uses-permission-sdk-23", "name" | "android:name") => {
                    manifest.uses_permissions.push(v.clone());
                }
                ("permission", "name" | "android:name") => {
                    manifest.permissions.push(v.clone());
                }
                _ => {}
            }
        }
        for child in elem.get_children() {
            if let axmldecoder::Node::Element(e) = child {
                collect_manifest(e, manifest);
            }
        }
    }
    collect_manifest(root, &mut manifest);
    Ok(manifest)
}
