//! APK previews and explicit exports. Contents are never executed.
use anyhow::{Context, Result, bail, ensure};
use image::{ImageReader, Limits};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT: usize = 2 * 1024 * 1024;
const HEX_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct Entry {
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub compressed_size: u64,
}

#[derive(Debug, Clone)]
pub struct Archive {
    pub entries: Vec<Entry>,
    path: PathBuf,
    file_size: u64,
    modified: Option<SystemTime>,
    checksums: Vec<u32>,
}

#[derive(Debug, Clone)]
pub enum Preview {
    Text {
        text: String,
        syntax: String,
        note: Option<String>,
    },
    Image {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    },
    Binary {
        text: String,
        note: String,
    },
}

impl Archive {
    /// Export original member bytes, independently of preview truncation or decoding.
    pub fn export(&self, index: usize, directory: &Path) -> Result<PathBuf> {
        let expected = self
            .entries
            .iter()
            .find(|entry| entry.index == index)
            .context("Unknown APK entry index")?;
        let file = File::open(&self.path).context("Cannot reopen APK")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.len() == self.file_size && metadata.modified().ok() == self.modified,
            "APK changed on disk; reopen it before exporting assets"
        );
        let mut zip = zip::ZipArchive::new(file).context("Invalid APK ZIP archive")?;
        let entry = zip.by_index(index).context("APK entry no longer exists")?;
        ensure!(
            entry.name() == expected.path
                && entry.size() == expected.size
                && entry.compressed_size() == expected.compressed_size
                && entry.crc32() == self.checksums[index],
            "APK entry changed; reopen the archive"
        );
        crate::export::write_stream(directory, &expected.path, entry, expected.size)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).context("Cannot open APK archive")?;
        let metadata = file.metadata()?;
        let mut zip = zip::ZipArchive::new(file).context("Invalid APK ZIP archive")?;
        let mut entries = Vec::new();
        let mut checksums = Vec::new();
        for index in 0..zip.len() {
            let entry = zip.by_index(index).context("Cannot read APK entry")?;
            checksums.push(entry.crc32());
            if entry.is_dir() {
                continue;
            }
            entries.push(Entry {
                index,
                path: entry.name().to_owned(),
                size: entry.size(),
                compressed_size: entry.compressed_size(),
            });
        }
        Ok(Self {
            entries,
            path: path.to_owned(),
            file_size: metadata.len(),
            modified: metadata.modified().ok(),
            checksums,
        })
    }

    /// `index` is the original ZIP member index exposed by `Entry`, not its list position.
    pub fn preview(&self, index: usize) -> Result<Preview> {
        let expected = self
            .entries
            .iter()
            .find(|e| e.index == index)
            .context("Unknown APK entry index")?;
        let file = File::open(&self.path).context("Cannot reopen APK")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.len() == self.file_size && metadata.modified().ok() == self.modified,
            "APK changed on disk; reopen it before previewing assets"
        );
        let mut zip = zip::ZipArchive::new(file).context("Invalid APK ZIP archive")?;
        let mut entry = zip.by_index(index).context("APK entry no longer exists")?;
        ensure!(
            entry.name() == expected.path
                && entry.size() == expected.size
                && entry.compressed_size() == expected.compressed_size
                && entry.crc32() == self.checksums[index],
            "APK entry changed; reopen the archive"
        );
        if expected.size > MAX_BYTES {
            let mut prefix = Vec::new();
            entry
                .by_ref()
                .take(HEX_BYTES as u64)
                .read_to_end(&mut prefix)
                .context("Cannot decompress APK entry")?;
            return Ok(binary(
                &prefix,
                format!(
                    "Entry is {} bytes; showing first {} bytes (8 MiB preview limit). Full-entry integrity was not checked.",
                    expected.size,
                    prefix.len()
                ),
            ));
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("Corrupt or unsupported APK entry")?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "APK entry exceeds decompression preview limit"
        );
        ensure!(
            bytes.len() as u64 == expected.size,
            "Corrupt APK entry: decompressed size mismatch"
        );
        let ext = Path::new(&expected.path)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        ) {
            let mut reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
            let mut limits = Limits::default();
            limits.max_image_width = Some(4096);
            limits.max_image_height = Some(4096);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            let decoded = reader
                .decode()
                .context("Cannot decode image (limit: 4096 x 4096 pixels and 64 MiB allocation)")?;
            let rgba = decoded.into_rgba8();
            return Ok(Preview::Image {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
            });
        }
        // Compiled XML is an archive format, not a text encoding. Decode it
        // independently of the DEX engine so previews work during/after a
        // failed class load, and for compiled XML outside res/ as well.
        if bytes.starts_with(&[0x03, 0x00, 0x08, 0x00]) {
            return match crate::native_resources::decode(&bytes) {
                Ok(text) => {
                    let (text, truncated) = truncate_text(text);
                    Ok(Preview::Text {
                        text,
                        syntax: "xml".into(),
                        note: truncated.then(|| "Text truncated at 2 MiB of UTF-8 output.".into()),
                    })
                }
                Err(error) => Ok(binary(
                    &bytes,
                    format!(
                        "Cannot decode compiled Android XML: {error}. Showing up to 4 KiB as hexadecimal."
                    ),
                )),
            };
        }
        match decode_text(&bytes) {
            Ok(Some(text)) => {
                let (text, truncated) = truncate_text(text);
                Ok(Preview::Text {
                    text,
                    syntax: syntax(&ext).to_owned(),
                    note: truncated.then(|| "Text truncated at 2 MiB of UTF-8 output.".to_owned()),
                })
            }
            Ok(None) => Ok(binary(
                &bytes,
                "Binary data; showing up to 4 KiB as hexadecimal.".into(),
            )),
            Err(error) => Ok(binary(
                &bytes,
                format!("Invalid text encoding: {error}. Showing up to 4 KiB as hexadecimal."),
            )),
        }
    }
}

fn syntax(ext: &str) -> &str {
    match ext {
        "json" => "json",
        "xml" | "svg" => "xml",
        "html" | "htm" => "html",
        "js" | "mjs" | "cjs" => "javascript",
        "css" => "css",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "yaml" | "yml" => "yaml",
        "smali" => "smali",
        "properties" | "ini" | "cfg" | "conf" => "ini",
        "md" => "markdown",
        "csv" | "tsv" => "csv",
        _ => "text",
    }
}

fn decode_text(bytes: &[u8]) -> Result<Option<String>> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        ensure!(bytes.len().is_multiple_of(2), "odd-length UTF-16 data");
        let little = bytes[0] == 0xff;
        let words = bytes[2..].as_chunks::<2>().0.iter().map(|b| {
            if little {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        });
        let mut text = String::new();
        for ch in char::decode_utf16(words) {
            text.push(ch.context("invalid UTF-16 surrogate")?);
        }
        text
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => bail!("invalid UTF-8"),
        }
    };
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
    {
        return Ok(None);
    }
    Ok(Some(text))
}

fn truncate_text(mut text: String) -> (String, bool) {
    if text.len() <= MAX_TEXT {
        return (text, false);
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

fn binary(bytes: &[u8], note: String) -> Preview {
    use std::fmt::Write;
    let mut text = String::new();
    for (line, chunk) in bytes[..bytes.len().min(HEX_BYTES)].chunks(16).enumerate() {
        let _ = write!(text, "{:08x}  ", line * 16);
        for i in 0..16 {
            if let Some(b) = chunk.get(i) {
                let _ = write!(text, "{b:02x} ");
            } else {
                text.push_str("   ");
            }
        }
        text.push(' ');
        for b in chunk {
            text.push(if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            });
        }
        text.push('\n');
    }
    Preview::Binary { text, note }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static ID: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let path = std::env::temp_dir().join(format!(
            "rdx-assets-{}-{}.apk",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
        zip.finish().unwrap();
        Fixture(path)
    }
    #[test]
    fn text_formats_and_no_extraction() {
        for (name, expected) in [
            ("../../assets/config.json", "json"),
            ("assets/icon.svg", "xml"),
            ("assets/page.html", "html"),
            ("assets/app.js", "javascript"),
            ("assets/a.css", "css"),
            ("assets/a.yaml", "yaml"),
            ("assets/a.properties", "ini"),
            ("assets/a.csv", "csv"),
        ] {
            let f = fixture(name, "\u{feff}hello Ελληνικά 🦀".as_bytes());
            let archive = Archive::open(&f.0).unwrap();
            assert_eq!(archive.entries[0].path, name);
            match archive.preview(0).unwrap() {
                Preview::Text { text, syntax, .. } => {
                    assert_eq!(syntax, expected);
                    assert_eq!(text, "hello Ελληνικά 🦀");
                }
                _ => panic!(),
            }
        }
    }
    #[test]
    fn unicode_and_invalid_text() {
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0x41, 0]).unwrap(),
            Some("A".into())
        );
        assert_eq!(
            decode_text(&[0xfe, 0xff, 0, 0x41]).unwrap(),
            Some("A".into())
        );
        assert!(decode_text(&[0xff, 0xfe, 0]).is_err());
        assert!(decode_text(&[0xff, 0xfe, 0, 0xd8]).is_err());
        assert!(decode_text(&[0xff]).is_err());
        let (text, truncated) = truncate_text("🦀".repeat(MAX_TEXT / 4 + 1));
        assert!(truncated);
        assert_eq!(text.len(), MAX_TEXT);
    }
    #[test]
    fn compressed_oversize_is_bounded() {
        let f = fixture("assets/large.txt", &vec![b'x'; MAX_BYTES as usize + 1]);
        let archive = Archive::open(&f.0).unwrap();
        assert!(archive.entries[0].compressed_size < 16384);
        match archive.preview(0).unwrap() {
            Preview::Binary { text, note } => {
                assert!(text.len() < 25000);
                assert!(note.contains("8 MiB"));
            }
            _ => panic!(),
        }
    }
    #[test]
    fn binary_is_safe_hex() {
        let f = fixture("assets/data.bin", &[0, 1, 2, 0xff]);
        match Archive::open(&f.0).unwrap().preview(0).unwrap() {
            Preview::Binary { text, .. } => assert!(text.contains("00 01 02 ff")),
            _ => panic!(),
        }
    }
    #[test]
    fn tiny_image() {
        let mut out = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        let f = fixture("assets/tiny.png", out.get_ref());
        match Archive::open(&f.0).unwrap().preview(0).unwrap() {
            Preview::Image {
                rgba,
                width,
                height,
            } => {
                assert_eq!((width, height), (2, 1));
                assert_eq!(rgba.len(), 8);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn changed_archive_is_rejected() {
        let f = fixture("assets/a.txt", b"hello");
        let archive = Archive::open(&f.0).unwrap();
        fs::write(&f.0, b"replaced").unwrap();
        assert!(
            archive
                .preview(0)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        let directory = crate::export::tests::Directory::new();
        assert!(
            archive
                .export(0, &directory.0)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }
    #[test]
    fn export_preserves_original_bytes_beyond_preview_limits() {
        let directory = crate::export::tests::Directory::new();
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        for (name, bytes) in [
            ("assets/tiny.png", png.into_inner()),
            ("assets/binary.bin", vec![0, 255, 0, 1, 2]),
            ("../../assets/large.txt", vec![b'x'; MAX_BYTES as usize + 1]),
        ] {
            let f = fixture(name, &bytes);
            let archive = Archive::open(&f.0).unwrap();
            let output = archive.export(0, &directory.0).unwrap();
            assert_eq!(output.parent(), Some(directory.0.as_path()));
            assert_eq!(fs::read(output).unwrap(), bytes);
        }
    }

    #[test]
    fn export_rejects_bad_crc_and_removes_partial_file() {
        let f = fixture("assets/bad.bin", b"original contents");
        let mut bytes = fs::read(&f.0).unwrap();
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        bytes[central + 16] ^= 0xff;
        fs::write(&f.0, bytes).unwrap();
        let archive = Archive::open(&f.0).unwrap();
        let directory = crate::export::tests::Directory::new();
        assert!(archive.export(0, &directory.0).is_err());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }
    #[test]
    fn image_dimensions_are_bounded() {
        let mut out = Cursor::new(Vec::new());
        image::RgbaImage::new(4097, 1)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        let f = fixture("assets/wide.png", out.get_ref());
        assert!(Archive::open(&f.0).unwrap().preview(0).is_err());
    }
    #[test]
    fn large_text_is_truncated_on_unicode_boundary() {
        let f = fixture("assets/large.txt", "🦀".repeat(MAX_TEXT / 4 + 1).as_bytes());
        match Archive::open(&f.0).unwrap().preview(0).unwrap() {
            Preview::Text { text, note, .. } => {
                assert_eq!(text.len(), MAX_TEXT);
                assert!(note.unwrap().contains("truncated"));
            }
            _ => panic!(),
        }
    }
    #[test]
    fn corrupt_archive_and_image_fail_clearly() {
        let f = fixture("assets/bad.png", b"this is not a PNG");
        assert!(Archive::open(&f.0).unwrap().preview(0).is_err());
        fs::write(&f.0, b"this is not a ZIP").unwrap();
        assert!(Archive::open(&f.0).is_err());
    }
}
