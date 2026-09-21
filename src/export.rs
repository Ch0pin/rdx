//! Bounded exports to a user-selected directory, without overwriting existing files.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_EXPORT_BYTES: u64 = 1024 * 1024 * 1024;

/// Use the basename only and produce a filename accepted on supported platforms.
fn safe_name(name: &str) -> String {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = basename
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    name = name.trim_matches([' ', '.']).to_owned();
    // Leave room for collision suffixes and avoid platform filename length limits.
    while name.len() > 200 {
        name.pop();
    }
    if name.is_empty() {
        name = "export".into();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|tail| {
                matches!(
                    tail,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
    if reserved {
        name.insert(0, '_');
    }
    name
}

pub fn write_source(directory: &Path, name: &str, source: &str) -> Result<PathBuf> {
    write_stream(directory, name, source.as_bytes(), source.len() as u64)
}

/// Read through EOF (including archive integrity checks), using a 64 KiB buffer.
pub(crate) fn write_stream(
    directory: &Path,
    name: &str,
    mut reader: impl Read,
    expected_size: u64,
) -> Result<PathBuf> {
    ensure!(
        expected_size <= MAX_EXPORT_BYTES,
        "Export exceeds the 1 GiB per-file limit"
    );
    ensure!(directory.is_dir(), "Export destination is not a directory");
    let name = safe_name(name);
    let base = Path::new(&name);
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let extension = base.extension().and_then(|s| s.to_str());
    let mut created = None;
    for suffix in 1..=10000 {
        let candidate = if suffix == 1 {
            name.clone()
        } else {
            match extension {
                Some(ext) => format!("{stem}-{suffix}.{ext}"),
                None => format!("{stem}-{suffix}"),
            }
        };
        let path = directory.join(candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                created = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Cannot create export file"),
        }
    }
    let Some((path, mut file)) = created else {
        bail!("Too many files with this export name");
    };
    let result = (|| -> Result<()> {
        let mut buffer = [0; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = reader
                .read(&mut buffer)
                .context("Cannot read export contents")?;
            if count == 0 {
                break;
            }
            total += count as u64;
            ensure!(
                total <= MAX_EXPORT_BYTES && total <= expected_size,
                "Export contents exceed expected size or 1 GiB limit"
            );
            file.write_all(&buffer[..count])
                .context("Cannot write export contents")?;
        }
        ensure!(
            total == expected_size,
            "Export contents have an unexpected size"
        );
        file.sync_all().context("Cannot finish export file")?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_file(&path) {
            return Err(error).context(format!(
                "Partial export remains at {}: {cleanup}",
                path.display()
            ));
        }
        return Err(error);
    }
    Ok(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static ID: AtomicU64 = AtomicU64::new(0);
    pub(crate) struct Directory(pub PathBuf);
    impl Directory {
        pub(crate) fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rdx-export-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn names_and_collisions_are_safe() {
        let dir = Directory::new();
        let first = write_source(&dir.0, "../../nested\\Test.java", "original").unwrap();
        let second = write_source(&dir.0, "Test.java", "second").unwrap();
        assert_eq!(first, dir.0.join("Test.java"));
        assert_eq!(second, dir.0.join("Test-2.java"));
        assert_eq!(fs::read_to_string(first).unwrap(), "original");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("LPT1"), "_LPT1");
        assert_eq!(safe_name(".."), "export");
        assert_eq!(safe_name("a:b\0.txt"), "a_b_.txt");
    }

    #[test]
    fn failures_remove_partial_output() {
        struct Broken(bool);
        impl Read for Broken {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("corrupt stream"));
                }
                self.0 = true;
                buffer[0] = 42;
                Ok(1)
            }
        }
        let dir = Directory::new();
        assert!(write_stream(&dir.0, "broken.bin", Broken(false), 5).is_err());
        assert!(write_stream(&dir.0, "short.bin", &b"x"[..], 2).is_err());
        assert!(write_stream(&dir.0, "long.bin", &b"xx"[..], 1).is_err());
        assert!(write_stream(&dir.0, "huge.bin", &b""[..], MAX_EXPORT_BYTES + 1).is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    }
}
