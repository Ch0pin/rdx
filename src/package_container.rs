//! Bounded, native APK and split-package container access.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

const MAX_NESTED_APKS: usize = 256;
const MAX_NESTED_APK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_NESTED_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_CONTAINER_METADATA: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ContainerEntry {
    pub index: usize,
    pub path: String,
    pub source_path: String,
    pub member_path: String,
    pub size: u64,
    pub compressed_size: u64,
    pub crc32: u32,
    pub is_apk_member: bool,
    source_index: usize,
    zip_index: usize,
}

#[derive(Debug)]
struct TemporaryApk(PathBuf);
impl Drop for TemporaryApk {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Debug)]
struct Source {
    path: PathBuf,
    outer_index: Option<usize>,
    outer_name: String,
    outer_size: u64,
    outer_crc: u32,
    _temporary: Option<TemporaryApk>,
}

#[derive(Debug)]
pub struct PackageContainer {
    path: PathBuf,
    original_size: u64,
    original_modified: Option<SystemTime>,
    sources: Vec<Source>,
    entries: Vec<ContainerEntry>,
    by_path: BTreeMap<String, usize>,
    summary: Option<String>,
}

#[derive(Debug)]
struct SplitMeta {
    package: String,
    version: String,
    split: Option<String>,
    config_for: Option<String>,
}

fn varint(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*pos).context("Truncated toc.pb varint")?;
        *pos += 1;
        if shift == 63 {
            ensure!(byte <= 1, "Invalid toc.pb varint overflow");
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    bail!("Invalid toc.pb varint")
}
fn fields(bytes: &[u8]) -> Result<Vec<(u32, u64, &[u8])>> {
    let mut pos = 0;
    let mut result: Vec<(u32, u64, &[u8])> = Vec::new();
    while pos < bytes.len() {
        ensure!(result.len() < 100_000, "toc.pb field budget exceeded");
        let key = varint(bytes, &mut pos)?;
        ensure!(
            (1..=(1 << 29) - 1).contains(&(key >> 3)),
            "Invalid toc.pb field number"
        );
        let number = (key >> 3) as u32;
        match key & 7 {
            0 => result.push((number, varint(bytes, &mut pos)?, &[])),
            1 => {
                let end = pos.checked_add(8).context("toc.pb overflow")?;
                result.push((
                    number,
                    0,
                    bytes.get(pos..end).context("Truncated toc.pb fixed64")?,
                ));
                pos = end;
            }
            2 => {
                let length = usize::try_from(varint(bytes, &mut pos)?)?;
                let end = pos.checked_add(length).context("toc.pb overflow")?;
                result.push((
                    number,
                    0,
                    bytes.get(pos..end).context("Truncated toc.pb bytes")?,
                ));
                pos = end;
            }
            5 => {
                let end = pos.checked_add(4).context("toc.pb overflow")?;
                result.push((
                    number,
                    0,
                    bytes.get(pos..end).context("Truncated toc.pb fixed32")?,
                ));
                pos = end;
            }
            _ => bail!("Unsupported toc.pb wire type"),
        }
    }
    Ok(result)
}
#[derive(Debug)]
struct TocVariant {
    number: u64,
    split_paths: Vec<String>,
    standalone_paths: Vec<String>,
}
fn parse_toc(bytes: &[u8]) -> Result<(String, Vec<TocVariant>)> {
    let mut package = String::new();
    let mut variants = Vec::new();
    for (number, _, value) in fields(bytes)? {
        match number {
            4 => package = std::str::from_utf8(value)?.to_owned(),
            1 => {
                let mut variant = TocVariant {
                    number: 0,
                    split_paths: Vec::new(),
                    standalone_paths: Vec::new(),
                };
                for (field, int, value) in fields(value)? {
                    match field {
                        3 => variant.number = int,
                        2 => {
                            for (set_field, _, set_value) in fields(value)? {
                                if set_field != 2 {
                                    continue;
                                }
                                let mut path = None;
                                let mut kind = None;
                                for (apk_field, _, apk_value) in fields(set_value)? {
                                    match apk_field {
                                        2 => {
                                            path = Some(std::str::from_utf8(apk_value)?.to_owned())
                                        }
                                        3 => kind = Some(3),
                                        4 => kind = Some(4),
                                        5..=9 => kind = Some(0),
                                        _ => {}
                                    }
                                }
                                if let Some(path) = path {
                                    match kind {
                                        Some(3) => variant.split_paths.push(path),
                                        Some(4) => variant.standalone_paths.push(path),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                variants.push(variant);
            }
            _ => {}
        }
    }
    ensure!(
        !package.is_empty() && !variants.is_empty(),
        "APKS toc.pb lacks package or variants"
    );
    Ok((package, variants))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
fn safe_split(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
fn read_zip_entry(zip: &mut zip::ZipArchive<File>, index: usize, limit: u64) -> Result<Vec<u8>> {
    let mut entry = zip.by_index(index)?;
    ensure!(
        entry.size() <= limit,
        "Archive member exceeds {} byte limit",
        limit
    );
    let mut bytes = Vec::new();
    entry.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == entry.size() && bytes.len() as u64 <= limit,
        "Archive member size mismatch"
    );
    Ok(bytes)
}
fn manifest_meta(bytes: &[u8]) -> Result<SplitMeta> {
    let xml = crate::native_resources::decode(bytes)
        .context("Cannot decode split AndroidManifest.xml")?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    loop {
        match reader.read_event()? {
            quick_xml::events::Event::Start(tag) | quick_xml::events::Event::Empty(tag)
                if tag.name().as_ref() == "manifest" =>
            {
                let mut package = None;
                let mut version = None;
                let mut split = None;
                let mut config_for = None;
                for attribute in tag.attributes() {
                    let attribute = attribute?;
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                        .into_owned();
                    match attribute.key.as_ref() {
                        "package" => package = Some(value),
                        "android:versionCode" | "versionCode" => version = Some(value),
                        "split" => split = Some(value),
                        "android:configForSplit" | "configForSplit" => config_for = Some(value),
                        _ => {}
                    }
                }
                return Ok(SplitMeta {
                    package: package.context("APK manifest has no package identity")?,
                    version: version.unwrap_or_default(),
                    split,
                    config_for,
                });
            }
            quick_xml::events::Event::Eof => bail!("APK manifest has no manifest element"),
            _ => {}
        }
    }
}
fn temporary_apk() -> Result<(File, TemporaryApk)> {
    let mut random = [0u8; 16];
    for _ in 0..8 {
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("Random temporary filename: {error}"))?;
        let name = format!(
            "rdx-split-{}-{:032x}.apk",
            std::process::id(),
            u128::from_le_bytes(random)
        );
        let path = std::env::temp_dir().join(name);
        let mut options = OpenOptions::new();
        options.write(true).read(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((file, TemporaryApk(path))),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("Could not allocate temporary split APK")
}

impl PackageContainer {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let path = path.canonicalize()?;
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        ensure!(
            matches!(extension.as_str(), "apk" | "apks" | "xapk"),
            "Choose an APK, APKS, or XAPK file"
        );
        crate::native_engine::check_zip_directory(&mut File::open(&path)?)?;
        let mut outer = zip::ZipArchive::new(file)?;
        let mut sources = Vec::new();
        let mut summary = None;
        let mut toc_package = None;
        let mut xapk_package = None;
        if extension == "apk" {
            sources.push(Source {
                path: path.clone(),
                outer_index: None,
                outer_name: String::new(),
                outer_size: metadata.len(),
                outer_crc: 0,
                _temporary: None,
            });
        } else {
            let mut candidates = Vec::new();
            let mut names = BTreeSet::new();
            let mut manifest_json = None;
            for index in 0..outer.len() {
                let entry = outer.by_index(index)?;
                ensure!(
                    entry.name().len() <= 4096 && safe_name(entry.name().trim_end_matches('/')),
                    "Unsafe container entry path"
                );
                ensure!(
                    names.insert(entry.name().to_ascii_lowercase()),
                    "Duplicate or case-colliding container entry"
                );
                if entry.name().eq_ignore_ascii_case("manifest.json") {
                    manifest_json = Some(index);
                }
                if !entry.is_dir() && entry.name().to_ascii_lowercase().ends_with(".apk") {
                    candidates.push((index, entry.name().to_owned(), entry.size(), entry.crc32()));
                }
            }
            ensure!(
                !candidates.is_empty(),
                "Package container contains no APK members"
            );
            ensure!(
                candidates.len() <= MAX_NESTED_APKS,
                "Package container has too many APK members"
            );
            if extension == "apks"
                && let Some(toc_index) = (0..outer.len())
                    .find(|&i| outer.by_index(i).is_ok_and(|e| e.name() == "toc.pb"))
            {
                let (package, mut variants) =
                    parse_toc(&read_zip_entry(&mut outer, toc_index, 16 * 1024 * 1024)?)?;
                let mut variant_numbers = BTreeSet::new();
                ensure!(
                    variants
                        .iter()
                        .all(|variant| variant_numbers.insert(variant.number)),
                    "APKS toc.pb has duplicate variant numbers"
                );
                variants.sort_by_key(|variant| std::cmp::Reverse(variant.number));
                let selected = variants
                    .iter()
                    .find(|variant| !variant.split_paths.is_empty())
                    .or_else(|| {
                        variants
                            .iter()
                            .find(|variant| variant.standalone_paths.len() == 1)
                    })
                    .context("APKS has no persistent split or standalone APK variant")?;
                let selected_paths = if !selected.split_paths.is_empty() {
                    &selected.split_paths
                } else {
                    &selected.standalone_paths
                };
                let mut selected_names = BTreeSet::new();
                for name in selected_paths {
                    ensure!(
                        safe_name(name) && selected_names.insert(name.to_ascii_lowercase()),
                        "APKS variant references unsafe or duplicate APK path"
                    );
                    ensure!(
                        candidates.iter().any(|(_, path, _, _)| path == name),
                        "APKS variant references missing APK: {name}"
                    );
                }
                candidates
                    .retain(|(_, name, _, _)| selected_names.contains(&name.to_ascii_lowercase()));
                summary = Some(format!(
                    "Bundletool variant {} analysis union (not a device install set), package {}, {} APKs",
                    selected.number,
                    package,
                    selected_paths.len()
                ));
                toc_package = Some(package);
            } else if extension == "xapk"
                && let Some(index) = manifest_json
            {
                let json: serde_json::Value =
                    serde_json::from_slice(&read_zip_entry(&mut outer, index, 2 * 1024 * 1024)?)?;
                xapk_package = json
                    .get("package_name")
                    .and_then(|x| x.as_str())
                    .map(str::to_owned);
                let mut referenced = BTreeSet::new();
                if let Some(apks) = json.get("split_apks").and_then(|x| x.as_array()) {
                    for apk in apks {
                        let name = apk
                            .get("file")
                            .or_else(|| apk.get("name"))
                            .and_then(|x| x.as_str())
                            .context("XAPK split_apks entry has no file")?;
                        ensure!(
                            safe_name(name) && referenced.insert(name.to_ascii_lowercase()),
                            "Unsafe or duplicate XAPK APK reference"
                        );
                    }
                } else if let Some(name) = json.get("package_name").and_then(|x| x.as_str()) {
                    let candidate = format!("{name}.apk");
                    if candidates.iter().any(|(_, path, _, _)| path == &candidate) {
                        referenced.insert(candidate.to_ascii_lowercase());
                    }
                }
                ensure!(
                    referenced.iter().all(|name| candidates
                        .iter()
                        .any(|(_, path, _, _)| path.to_ascii_lowercase() == *name)),
                    "XAPK manifest references a missing APK"
                );
                // The JSON format varies among producers. Its references are
                // checked, but actual APK manifests determine the full set.
            }
            let mut total = 0u64;
            for (index, name, size, crc) in candidates {
                ensure!(
                    size <= MAX_NESTED_APK_BYTES,
                    "Nested APK exceeds 2 GiB limit"
                );
                total = total
                    .checked_add(size)
                    .context("Nested APK byte count overflow")?;
                ensure!(
                    total <= MAX_NESTED_TOTAL_BYTES,
                    "Nested APK set exceeds 4 GiB limit"
                );
                let (mut output, temporary) = temporary_apk()?;
                {
                    let mut entry = outer.by_index(index)?;
                    let copied = std::io::copy(&mut entry.by_ref().take(size + 1), &mut output)?;
                    ensure!(copied == size, "Nested APK size mismatch");
                }
                output.flush()?;
                crate::native_engine::check_zip_directory(&mut File::open(&temporary.0)?)
                    .with_context(|| format!("Invalid nested APK {name}"))?;
                sources.push(Source {
                    path: temporary.0.clone(),
                    outer_index: Some(index),
                    outer_name: name,
                    outer_size: size,
                    outer_crc: crc,
                    _temporary: Some(temporary),
                });
            }
        }
        let mut identities = Vec::new();
        for source in &sources {
            let mut zip = zip::ZipArchive::new(File::open(&source.path)?)?;
            let index = if extension == "apk" {
                None
            } else {
                zip.file_names()
                    .position(|name| name == "AndroidManifest.xml")
            };
            if let Some(index) = index {
                let bytes = read_zip_entry(&mut zip, index, 32 * 1024 * 1024)?;
                identities.push(
                    manifest_meta(&bytes)
                        .with_context(|| format!("Identifying {}", source.outer_name))?,
                );
            } else {
                ensure!(
                    extension == "apk",
                    "Nested APK lacks AndroidManifest.xml: {}",
                    source.outer_name
                );
                identities.push(SplitMeta {
                    package: String::new(),
                    version: String::new(),
                    split: None,
                    config_for: None,
                });
            }
        }
        if extension != "apk" {
            let base_indices: Vec<_> = identities
                .iter()
                .enumerate()
                .filter(|(_, meta)| meta.split.is_none())
                .map(|(i, _)| i)
                .collect();
            ensure!(
                base_indices.len() == 1,
                "Container has {} base APK alternatives; provide a device-targeted APKS/XAPK with one base",
                base_indices.len()
            );
            let base = base_indices[0];
            let package = &identities[base].package;
            if let Some(expected) = &toc_package {
                ensure!(
                    expected == package,
                    "APKS toc.pb package does not match base APK manifest"
                );
            }
            if let Some(expected) = &xapk_package {
                ensure!(
                    expected == package,
                    "XAPK manifest.json package does not match base APK manifest"
                );
            }
            let version = &identities[base].version;
            let mut split_names = BTreeSet::new();
            for (index, meta) in identities.iter().enumerate() {
                ensure!(
                    &meta.package == package && &meta.version == version,
                    "Split {} has a different package/version identity",
                    sources[index].outer_name
                );
                if let Some(split) = &meta.split {
                    ensure!(
                        safe_split(split) && split_names.insert(split.to_ascii_lowercase()),
                        "Duplicate or unsafe split identity: {split}"
                    );
                    if let Some(parent) = &meta.config_for {
                        ensure!(
                            parent == "base"
                                || identities
                                    .iter()
                                    .any(|candidate| candidate.split.as_deref() == Some(parent)),
                            "Config split {split} has missing feature parent {parent}"
                        );
                    }
                }
            }
            // A standalone/universal build alongside split APKs is an alternate build.
            ensure!(
                !(identities.len() > 1 && sources[base].outer_name.contains("standalone")),
                "APKS contains standalone and split alternatives; provide one device-targeted variant"
            );
            if summary.is_none() {
                summary = Some(format!(
                    "{} analysis union: one base and {} validated splits for {} version {}",
                    extension.to_ascii_uppercase(),
                    identities.len() - 1,
                    package,
                    version
                ));
            }
            if base != 0 {
                sources.swap(0, base);
                identities.swap(0, base);
            }
        }
        let mut entries = Vec::new();
        let mut by_path = BTreeMap::new();
        let mut metadata_bytes = 0usize;
        for (source_index, source) in sources.iter().enumerate() {
            let mut zip = zip::ZipArchive::new(File::open(&source.path)?)?;
            for zip_index in 0..zip.len() {
                let entry = zip.by_index(zip_index)?;
                ensure!(
                    entry.name().len() <= 4096 && safe_name(entry.name().trim_end_matches('/')),
                    "Unsafe APK member path"
                );
                if entry.is_dir() {
                    continue;
                }
                let member_path = entry.name().to_owned();
                let path = if source_index == 0 {
                    member_path.clone()
                } else {
                    format!(
                        "splits/{}/{member_path}",
                        identities[source_index].split.as_deref().unwrap()
                    )
                };
                metadata_bytes = metadata_bytes
                    .checked_add(path.len() + source.outer_name.len() + 96)
                    .context("Archive metadata overflow")?;
                ensure!(
                    metadata_bytes <= MAX_CONTAINER_METADATA,
                    "Archive inventory exceeds 32 MiB metadata limit"
                );
                ensure!(
                    !by_path.contains_key(&path),
                    "Duplicate APK member path: {path}"
                );
                let index = entries.len();
                by_path.insert(path.clone(), index);
                entries.push(ContainerEntry {
                    index,
                    path,
                    source_path: source.outer_name.clone(),
                    member_path,
                    size: entry.size(),
                    compressed_size: entry.compressed_size(),
                    crc32: entry.crc32(),
                    is_apk_member: true,
                    source_index,
                    zip_index,
                });
            }
        }
        if extension == "xapk" {
            let source_index = sources.len();
            sources.push(Source {
                path: path.clone(),
                outer_index: None,
                outer_name: "container".into(),
                outer_size: metadata.len(),
                outer_crc: 0,
                _temporary: None,
            });
            for zip_index in 0..outer.len() {
                let entry = outer.by_index(zip_index)?;
                if entry.is_dir() || entry.name().to_ascii_lowercase().ends_with(".apk") {
                    continue;
                }
                let member_path = entry.name().to_owned();
                let logical = format!("container/{member_path}");
                metadata_bytes = metadata_bytes
                    .checked_add(logical.len() + 96)
                    .context("Archive metadata overflow")?;
                ensure!(
                    metadata_bytes <= MAX_CONTAINER_METADATA,
                    "Archive inventory exceeds 32 MiB metadata limit"
                );
                let index = entries.len();
                ensure!(
                    !by_path.contains_key(&logical),
                    "Duplicate logical package path: {logical}"
                );
                by_path.insert(logical.clone(), index);
                entries.push(ContainerEntry {
                    index,
                    path: logical,
                    source_path: "container".into(),
                    member_path,
                    size: entry.size(),
                    compressed_size: entry.compressed_size(),
                    crc32: entry.crc32(),
                    is_apk_member: false,
                    source_index,
                    zip_index,
                });
            }
        }
        Ok(Arc::new(Self {
            path,
            original_size: metadata.len(),
            original_modified: metadata.modified().ok(),
            sources,
            entries,
            by_path,
            summary,
        }))
    }
    pub fn entries(&self) -> &[ContainerEntry] {
        &self.entries
    }
    pub fn selection_summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }
    pub fn source_path(&self) -> &Path {
        &self.path
    }
    fn check_original(&self, source: &Source) -> Result<()> {
        let metadata = File::open(&self.path)?.metadata()?;
        ensure!(
            metadata.len() == self.original_size
                && metadata.modified().ok() == self.original_modified,
            "Package archive changed on disk; reopen it"
        );
        if let Some(index) = source.outer_index {
            let mut outer = zip::ZipArchive::new(File::open(&self.path)?)?;
            let entry = outer.by_index(index)?;
            ensure!(
                entry.name() == source.outer_name
                    && entry.size() == source.outer_size
                    && entry.crc32() == source.outer_crc,
                "Nested APK changed on disk; reopen the package"
            );
        }
        Ok(())
    }
    pub fn copy_member(&self, index: usize, writer: &mut impl Write, max_bytes: u64) -> Result<()> {
        let expected = self
            .entries
            .get(index)
            .context("Unknown archive member index")?;
        ensure!(
            expected.size <= max_bytes,
            "Archive member exceeds export limit"
        );
        let source = &self.sources[expected.source_index];
        self.check_original(source)?;
        let mut zip = zip::ZipArchive::new(File::open(&source.path)?)?;
        let mut entry = zip.by_index(expected.zip_index)?;
        ensure!(
            entry.name() == expected.member_path
                && entry.size() == expected.size
                && entry.compressed_size() == expected.compressed_size
                && entry.crc32() == expected.crc32,
            "Archive member changed; reopen it"
        );
        let copied = std::io::copy(&mut entry.by_ref().take(max_bytes + 1), writer)?;
        ensure!(
            copied == expected.size && copied <= max_bytes,
            "Archive member decompression size mismatch"
        );
        Ok(())
    }
    pub fn read_member(&self, index: usize, max_bytes: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.copy_member(index, &mut bytes, max_bytes)?;
        Ok(bytes)
    }
    pub fn read_path(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        self.read_member(
            *self
                .by_path
                .get(path)
                .with_context(|| format!("Resource not found: {path}"))?,
            max_bytes,
        )
    }
    pub fn read_prefix(&self, index: usize, limit: usize) -> Result<Vec<u8>> {
        let expected = self
            .entries
            .get(index)
            .context("Unknown archive member index")?;
        let source = &self.sources[expected.source_index];
        self.check_original(source)?;
        let mut zip = zip::ZipArchive::new(File::open(&source.path)?)?;
        let mut entry = zip.by_index(expected.zip_index)?;
        ensure!(
            entry.name() == expected.member_path
                && entry.size() == expected.size
                && entry.crc32() == expected.crc32,
            "Archive member changed; reopen it"
        );
        let mut bytes = Vec::new();
        entry.by_ref().take(limit as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DecompilerEngine, NativeEngine};
    use std::io::Cursor;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn temporary_container(extension: &str, entries: &[(&str, &[u8])]) -> (PathBuf, TemporaryApk) {
        let (file, mut cleanup) = temporary_apk().unwrap();
        drop(file);
        let path = cleanup.0.with_extension(extension);
        fs::rename(&cleanup.0, &path).unwrap();
        cleanup.0 = path.clone();
        fs::write(&path, zip_bytes(entries)).unwrap();
        (path, cleanup)
    }
    fn base() -> Vec<u8> {
        zip_bytes(&[
            (
                "AndroidManifest.xml",
                br#"<manifest package="sample.test" android:versionCode="1"/>"#,
            ),
            ("classes.dex", include_bytes!("../tests/fixtures/hello.dex")),
        ])
    }
    fn split(id: &str, include_dex: bool) -> Vec<u8> {
        let manifest =
            format!("<manifest package=\"sample.test\" android:versionCode=\"1\" split=\"{id}\"/>");
        let mut entries = vec![
            ("AndroidManifest.xml", manifest.as_bytes()),
            ("res/raw/note.txt", b"split asset".as_slice()),
        ];
        if include_dex {
            entries.push((
                "classes.dex",
                include_bytes!("../tests/fixtures/hello.dex").as_slice(),
            ));
        }
        zip_bytes(&entries)
    }
    fn pb_varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        while value >= 128 {
            out.push((value as u8) | 128);
            value >>= 7;
        }
        out.push(value as u8);
        out
    }
    fn pb_bytes(field: u64, value: &[u8]) -> Vec<u8> {
        let mut out = pb_varint(field << 3 | 2);
        out.extend(pb_varint(value.len() as u64));
        out.extend(value);
        out
    }
    fn pb_int(field: u64, value: u64) -> Vec<u8> {
        let mut out = pb_varint(field << 3);
        out.extend(pb_varint(value));
        out
    }
    fn toc_variant(number: u64, paths: &[(&str, u64)]) -> Vec<u8> {
        let mut variant = pb_int(3, number);
        let mut set = Vec::new();
        for (path, kind) in paths {
            let mut apk = pb_bytes(2, path.as_bytes());
            apk.extend(pb_bytes(*kind, &[]));
            set.extend(pb_bytes(2, &apk));
        }
        variant.extend(pb_bytes(2, &set));
        pb_bytes(1, &variant)
    }
    #[test]
    fn xapk_keeps_all_validated_splits_and_outer_assets() {
        let base = base();
        let feature = split("feature", false);
        let (path, _cleanup) = temporary_container(
            "xapk",
            &[
                ("base.apk", &base),
                ("feature.apk", &feature),
                ("main.obb", b"opaque"),
            ],
        );
        let container = PackageContainer::open(&path).unwrap();
        assert!(
            container
                .entries()
                .iter()
                .any(|e| e.path == "splits/feature/res/raw/note.txt")
        );
        assert_eq!(
            container.read_path("container/main.obb", 16).unwrap(),
            b"opaque"
        );
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open_prepared(container).unwrap();
        assert_eq!(project.classes, ["sample.Hello"]);
        assert!(
            project
                .resources
                .contains(&"splits/feature/AndroidManifest.xml".to_owned())
        );
    }
    #[test]
    fn apks_selects_one_persistent_variant_and_rejects_duplicate_code() {
        let base = base();
        let feature = split("feature", false);
        let mut toc = pb_bytes(4, b"sample.test");
        toc.extend(toc_variant(1, &[("old.apk", 4)]));
        toc.extend(toc_variant(3, &[("base.apk", 3), ("feature.apk", 3)]));
        toc.extend(toc_variant(9, &[("instant.apk", 5)]));
        let (path, _cleanup) = temporary_container(
            "apks",
            &[
                ("toc.pb", &toc),
                ("old.apk", &base),
                ("base.apk", &base),
                ("feature.apk", &feature),
                ("instant.apk", &base),
            ],
        );
        let container = PackageContainer::open(&path).unwrap();
        assert!(container.selection_summary().unwrap().contains("variant 3"));
        assert!(
            !container
                .entries()
                .iter()
                .any(|e| e.source_path == "old.apk" || e.source_path == "instant.apk")
        );
        let mut engine = NativeEngine::start().unwrap();
        assert_eq!(
            engine.open_prepared(container).unwrap().classes,
            ["sample.Hello"]
        );
        let duplicate = split("feature", true);
        let (path, _cleanup) =
            temporary_container("xapk", &[("base.apk", &base), ("feature.apk", &duplicate)]);
        let error = format!(
            "{:#}",
            NativeEngine::start().unwrap().open(&path).unwrap_err()
        );
        assert!(error.contains("base.apk") && error.contains("feature.apk"));
    }
    #[test]
    fn malformed_toc_and_duplicate_base_are_rejected() {
        let base = base();
        let (path, _cleanup) =
            temporary_container("apks", &[("toc.pb", &[0xff]), ("base.apk", &base)]);
        assert!(PackageContainer::open(&path).is_err());
        let (path, _cleanup) =
            temporary_container("xapk", &[("base.apk", &base), ("alternative.apk", &base)]);
        assert!(
            PackageContainer::open(&path)
                .unwrap_err()
                .to_string()
                .contains("base APK alternatives")
        );
    }
    #[test]
    fn failed_reopen_clears_old_classes_and_plain_apk_ignores_bad_manifest() {
        let (plain_path, _cleanup) = temporary_container(
            "apk",
            &[
                ("AndroidManifest.xml", b"bad XML"),
                ("classes.dex", include_bytes!("../tests/fixtures/hello.dex")),
            ],
        );
        let mut engine = NativeEngine::start().unwrap();
        assert_eq!(engine.open(&plain_path).unwrap().classes, ["sample.Hello"]);
        let (bad_path, _cleanup) =
            temporary_container("xapk", &[("a.apk", &base()), ("b.apk", &base())]);
        assert!(engine.open(&bad_path).is_err());
        assert!(engine.decompile("sample.Hello").is_err());
        assert!(engine.prepared_container().is_none());
    }
    #[test]
    fn outer_asset_cannot_shadow_logical_apk_member() {
        let inner = zip_bytes(&[
            (
                "AndroidManifest.xml",
                br#"<manifest package="sample.test" android:versionCode="1"/>"#,
            ),
            ("classes.dex", include_bytes!("../tests/fixtures/hello.dex")),
            ("container/main.obb", b"inner"),
        ]);
        let (path, _cleanup) =
            temporary_container("xapk", &[("base.apk", &inner), ("main.obb", b"outer")]);
        assert!(
            PackageContainer::open(&path)
                .unwrap_err()
                .to_string()
                .contains("Duplicate logical package path")
        );
    }
}
