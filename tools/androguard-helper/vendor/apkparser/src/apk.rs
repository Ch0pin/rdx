//! Main APK parser - mirrors apkparser.APK.

use crate::apkm::{self, ApkmArchive};
use crate::error::Result;
use crate::manifest::{parse_manifest, AndroidManifest};
use crate::permissions::{load_permissions, Permissions};
use crate::signature::ApkSignature;
use crate::utils::is_android_raw;
use crate::zip::ZipArchive;
use std::path::Path;

/// Options for APK parsing (mirrors OPTION_*).
#[derive(Clone, Copy, Debug, Default)]
pub struct ApkOptions {
    pub axml: bool,
    pub signature: bool,
    pub permission: bool,
}

impl ApkOptions {
    pub const AXML: Self = Self {
        axml: true,
        signature: false,
        permission: false,
    };
    pub const SIGNATURE: Self = Self {
        axml: false,
        signature: true,
        permission: false,
    };
    pub const PERMISSION: Self = Self {
        axml: false,
        signature: false,
        permission: true,
    };
    pub fn with_axml(mut self, on: bool) -> Self {
        self.axml = on;
        self
    }
    pub fn with_signature(mut self, on: bool) -> Self {
        self.signature = on;
        self
    }
    pub fn with_permission(mut self, on: bool) -> Self {
        self.permission = on;
        self
    }
}

/// APK parser - main entry (mirrors Python APK class).
pub struct Apk {
    zip: ZipArchive,
    pub signature: Option<ApkSignature>,
    pub manifest: Option<AndroidManifest>,
    pub permissions: Option<Permissions>,
    /// Permissions JSON base path (for loading AOSP permissions by API level).
    pub permissions_base_path: Option<std::path::PathBuf>,
}

impl Apk {
    /// Create APK parser from raw bytes.
    ///
    /// If `apk` is an APKM container, automatically unwraps to the base APK so
    /// callers can treat APKM like a normal package for manifest / DEX / signature.
    /// Other ZIPs (including signature-only or evasion-malformed APKs) are parsed as-is.
    pub fn from_bytes(apk: &[u8], options: ApkOptions) -> Result<Self> {
        let data = if apkm::looks_like_apkm(apk) {
            ApkmArchive::from_bytes(apk)?.base_apk_bytes()?
        } else {
            apk.to_vec()
        };
        Self::from_apk_bytes(data, options)
    }

    /// Like [`from_bytes`], but never unwraps APKM — `data` must already be a plain APK.
    pub fn from_apk_bytes(data: Vec<u8>, options: ApkOptions) -> Result<Self> {
        let zip = ZipArchive::parse(data)?;
        let mut signature = None;
        let mut manifest = None;
        let mut permissions = None;

        if options.axml {
            if let Ok(manifest_bytes) = zip.read("AndroidManifest.xml") {
                match parse_manifest(&manifest_bytes) {
                    Ok(m) => manifest = Some(m),
                    Err(_) => {}
                }
            }
        }

        if options.signature {
            signature = Some(ApkSignature::from_zip(zip.data(), &zip)?);
        }

        if options.permission {
            if let Some(ref m) = manifest {
                let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../apkparser/permissions/ressources/aosp_permissions");
                let api = m.target_sdk_version.unwrap_or(16);
                if let Ok(aosp) = load_permissions(&base, api) {
                    permissions = Some(Permissions::new(m.uses_permissions.clone(), aosp));
                }
            }
        }

        Ok(Self {
            zip,
            signature,
            manifest,
            permissions,
            permissions_base_path: None,
        })
    }

    /// Open from path; accepts `.apk` or `.apkm` (APKM → base APK).
    pub fn from_path(path: &Path, options: ApkOptions) -> Result<Self> {
        let raw = std::fs::read(path)?;
        Self::from_bytes(&raw, options)
    }

    /// Open an APKM and return `(Apk for base, ApkmArchive)` for split access.
    pub fn from_apkm(raw: &[u8], options: ApkOptions) -> Result<(Self, ApkmArchive)> {
        let archive = ApkmArchive::from_bytes(raw)?;
        let base = archive.base_apk_bytes()?;
        Ok((Self::from_apk_bytes(base, options)?, archive))
    }

    /// Return true if this looks like an APK (ZIP with AndroidManifest.xml).
    pub fn is_apk(&self) -> bool {
        self.zip.contains("AndroidManifest.xml")
            || is_android_raw(self.zip.data()) == Some("APK")
    }

    /// List file names inside the APK (like get_files / namelist).
    pub fn get_files(&self) -> Vec<String> {
        self.zip.names()
    }

    /// Read a file by name (like get_file).
    pub fn get_file(&self, name: &str) -> Result<Vec<u8>> {
        self.zip.read(name)
    }

    /// Return raw AndroidManifest.xml bytes if present.
    pub fn get_android_manifest_bytes(&self) -> Option<Vec<u8>> {
        self.zip.read("AndroidManifest.xml").ok()
    }

    /// Return parsed Android manifest if AXML option was set and parsing succeeded.
    pub fn get_android_manifest(&self) -> Option<&AndroidManifest> {
        self.manifest.as_ref()
    }

    /// Return permissions helper if PERMISSION option was set.
    pub fn get_permissions(&self) -> Option<&Permissions> {
        self.permissions.as_ref()
    }

    /// Return signature parser if SIGNATURE option was set.
    pub fn get_signature(&self) -> Option<&ApkSignature> {
        self.signature.as_ref()
    }

    /// Return mutable signature parser (for lazy parsing of v2/v3 blocks).
    pub fn get_signature_mut(&mut self) -> Option<&mut ApkSignature> {
        self.signature.as_mut()
    }
}
