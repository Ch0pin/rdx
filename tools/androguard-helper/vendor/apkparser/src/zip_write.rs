//! APK/ZIP writer for repacking entries.

use std::io::{Cursor, Write};

use crate::error::{Error, Result};

/// Compression mode for a ZIP entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// Store without compression (method 0).
    Stored,
    /// Deflate compression (method 8).
    Deflated,
}

/// Builder for creating APK/ZIP archives in memory.
#[derive(Default)]
pub struct ApkWriter {
    entries: Vec<ZipEntryWrite>,
}

struct ZipEntryWrite {
    name: String,
    data: Vec<u8>,
    compression: Compression,
}

impl ApkWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry. `compress` selects Deflated vs Stored.
    pub fn add_entry(&mut self, name: impl Into<String>, data: &[u8], compress: bool) {
        self.entries.push(ZipEntryWrite {
            name: name.into(),
            data: data.to_vec(),
            compression: if compress {
                Compression::Deflated
            } else {
                Compression::Stored
            },
        });
    }

    /// Add an entry with explicit compression mode.
    pub fn add_entry_with_compression(
        &mut self,
        name: impl Into<String>,
        data: &[u8],
        compression: Compression,
    ) {
        self.entries.push(ZipEntryWrite {
            name: name.into(),
            data: data.to_vec(),
            compression,
        });
    }

    /// Build the ZIP archive bytes.
    pub fn finish(self) -> Result<Vec<u8>> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options_stored = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let options_deflated = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            for entry in &self.entries {
                let options = match entry.compression {
                    Compression::Stored => options_stored,
                    Compression::Deflated => options_deflated,
                };
                zip.start_file(&entry.name, options)
                    .map_err(Error::Zip)?;
                zip.write_all(&entry.data).map_err(Error::Io)?;
            }
            zip.finish().map_err(Error::Zip)?;
        }
        Ok(buf.into_inner())
    }
}

/// Return true if `path` should remain uncompressed per Apktool `doNotCompress` rules.
pub fn should_compress(path: &str, do_not_compress: &[String]) -> bool {
    if do_not_compress.iter().any(|rule| path == rule) {
        return false;
    }
    if let Some(ext) = path.rsplit('.').next() {
        if do_not_compress.iter().any(|rule| rule == ext) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::ZipEntry;

    #[test]
    fn roundtrip_zip() {
        let mut writer = ApkWriter::new();
        writer.add_entry("AndroidManifest.xml", b"<manifest/>", false);
        writer.add_entry("classes.dex", &[0x64, 0x65, 0x78, 0x0a], true);
        let bytes = writer.finish().unwrap();
        let zip = ZipEntry::parse(&bytes).unwrap();
        assert!(zip.contains("AndroidManifest.xml"));
        assert!(zip.contains("classes.dex"));
    }

    #[test]
    fn do_not_compress_rules() {
        assert!(!should_compress("resources.arsc", &["arsc".into()]));
        assert!(!should_compress("assets/foo.dat", &["assets/foo.dat".into()]));
        assert!(should_compress("classes.dex", &["arsc".into()]));
    }
}
