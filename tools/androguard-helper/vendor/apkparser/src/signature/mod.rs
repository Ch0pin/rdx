//! APK signature parsing (v1/JAR, v2, v3).

mod apk_sig_block;
mod utils;
mod v1;
mod v2;
mod v3;

pub use apk_sig_block::{
    parse_apk_sig_block, parse_apk_sig_block_at, APK_SIG_KEY_V2_SIGNATURE, APK_SIG_KEY_V3_SIGNATURE,
};
pub use v2::{parse_v2_signing_block, ApkV2SignedData, ApkV2Signer};
pub use v3::{parse_v3_signing_block, ApkV3SignedData, ApkV3Signer};

use crate::zip::ZipArchive;
use std::collections::HashMap;

/// APK signature parser: parses v1 (JAR), v2, and v3 signatures from an APK.
pub struct ApkSignature {
    is_signed_v2: bool,
    is_signed_v3: bool,
    v2_blocks: HashMap<u32, Vec<u8>>,
    v2_signing_data: Option<Vec<ApkV2Signer>>,
    v3_signing_data: Option<Vec<ApkV3Signer>>,
    zip_names: Vec<String>,
    zip_file_contents: HashMap<String, Vec<u8>>,
}

impl ApkSignature {
    pub fn from_bytes(apk: &[u8]) -> crate::Result<Self> {
        let (is_signed_v2, is_signed_v3, v2_blocks) = parse_apk_sig_block(apk)?;
        let zip = ZipArchive::parse_slice(apk)?;
        Self::from_zip_and_blocks(&zip, is_signed_v2, is_signed_v3, v2_blocks)
    }

    /// Build from an already-parsed [`ZipArchive`] (avoids re-reading the archive).
    /// Only `META-INF` PKCS7 / `.SF` entries are inflated for v1.
    pub fn from_zip(apk: &[u8], zip: &ZipArchive) -> crate::Result<Self> {
        let (is_signed_v2, is_signed_v3, v2_blocks) = parse_apk_sig_block(apk)?;
        Self::from_zip_and_blocks(zip, is_signed_v2, is_signed_v3, v2_blocks)
    }

    fn from_zip_and_blocks(
        zip: &ZipArchive,
        is_signed_v2: bool,
        is_signed_v3: bool,
        v2_blocks: HashMap<u32, Vec<u8>>,
    ) -> crate::Result<Self> {
        let zip_names = zip.names();
        let mut zip_file_contents: HashMap<String, Vec<u8>> = HashMap::new();
        for name in &zip_names {
            if is_v1_signature_file(name) {
                if let Ok(data) = zip.read(name) {
                    zip_file_contents.insert(name.clone(), data);
                }
            }
        }
        Ok(Self {
            is_signed_v2,
            is_signed_v3,
            v2_blocks,
            v2_signing_data: None,
            v3_signing_data: None,
            zip_names,
            zip_file_contents,
        })
    }

    pub fn is_signed(&self) -> bool {
        self.is_signed_v1() || self.is_signed_v2() || self.is_signed_v3()
    }

    pub fn is_signed_v1(&self) -> bool {
        self.get_signature_name().is_some()
    }

    pub fn is_signed_v2(&self) -> bool {
        self.is_signed_v2
    }

    pub fn is_signed_v3(&self) -> bool {
        self.is_signed_v3
    }

    pub fn get_signature_name(&self) -> Option<String> {
        self.get_signature_names().into_iter().next()
    }

    pub fn get_signature_names(&self) -> Vec<String> {
        v1::get_signature_names(&self.zip_names)
    }

    pub fn get_certificate_der(&self, filename: &str) -> crate::Result<Option<Vec<u8>>> {
        let pkcs7_bytes = self
            .zip_file_contents
            .get(filename)
            .cloned()
            .ok_or_else(|| crate::error::Error::Parse(format!("file not found: {}", filename)))?;
        let base = filename.rsplitn(2, '.').nth(1).unwrap_or(filename);
        let sf_name = format!("{}.SF", base);
        let sf_bytes = self
            .zip_file_contents
            .get(&sf_name)
            .cloned()
            .unwrap_or_default();
        v1::get_certificate_der_from_pkcs7(&pkcs7_bytes, &sf_bytes)
    }

    fn ensure_v2_signing_data(&mut self) -> crate::Result<()> {
        if self.v2_signing_data.is_none() && self.is_signed_v2 {
            if let Some(block) = self.v2_blocks.get(&APK_SIG_KEY_V2_SIGNATURE) {
                self.v2_signing_data = Some(v2::parse_v2_signing_block(block)?);
            }
        }
        Ok(())
    }

    fn ensure_v3_signing_data(&mut self) -> crate::Result<()> {
        if self.v3_signing_data.is_none() && self.is_signed_v3 {
            if let Some(block) = self.v2_blocks.get(&APK_SIG_KEY_V3_SIGNATURE) {
                self.v3_signing_data = Some(v3::parse_v3_signing_block(block)?);
            }
        }
        Ok(())
    }

    pub fn get_public_keys_der_v2(&mut self) -> crate::Result<Vec<Vec<u8>>> {
        self.ensure_v2_signing_data()?;
        Ok(self
            .v2_signing_data
            .as_ref()
            .map(|signers| signers.iter().map(|s| s.public_key.clone()).collect())
            .unwrap_or_default())
    }

    pub fn get_public_keys_der_v3(&mut self) -> crate::Result<Vec<Vec<u8>>> {
        self.ensure_v3_signing_data()?;
        Ok(self
            .v3_signing_data
            .as_ref()
            .map(|signers| signers.iter().map(|s| s.public_key.clone()).collect())
            .unwrap_or_default())
    }

    pub fn get_certificates_der_v2(&mut self) -> crate::Result<Vec<Vec<u8>>> {
        self.ensure_v2_signing_data()?;
        let mut certs = Vec::new();
        if let Some(ref signers) = self.v2_signing_data {
            for s in signers {
                certs.extend(s.signed_data.certificates.clone());
            }
        }
        Ok(certs)
    }

    pub fn get_certificates_der_v3(&mut self) -> crate::Result<Vec<Vec<u8>>> {
        self.ensure_v3_signing_data()?;
        let mut certs = Vec::new();
        if let Some(ref signers) = self.v3_signing_data {
            for s in signers {
                certs.extend(s.signed_data.certificates.clone());
            }
        }
        Ok(certs)
    }

    pub fn get_signatures(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for name in &self.get_signature_names() {
            if let Some(data) = self.zip_file_contents.get(name) {
                out.push(data.clone());
            }
        }
        out
    }
}

fn is_v1_signature_file(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if !upper.starts_with("META-INF/") {
        return false;
    }
    upper.ends_with(".RSA")
        || upper.ends_with(".DSA")
        || upper.ends_with(".EC")
        || upper.ends_with(".SF")
}
