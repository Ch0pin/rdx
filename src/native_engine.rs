//! Native DEX backend. Unsupported reconstruction is explicit, never fabricated source.
#[path = "native_disassembly.rs"]
pub(crate) mod disassembly;
#[path = "native_implementations.rs"]
mod implementations;
use crate::{
    engine::{DecompiledCode, DecompilerEngine, DefinitionName, DefinitionNameIndex, Project},
    native_dex::{self, DexClass},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const MAX_DEX_BYTES: u64 = 256 * 1024 * 1024;
// Parsed code and symbol tables are wider than their packed DEX representation.
// Keep this separate from the input-byte limit and bounded for hostile archives.
const MAX_RETAINED_DEX_BYTES: usize = 1024 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 100_000;
const MAX_CLASSES: usize = 250_000;
const MAX_ARCHIVE_METADATA: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct UnsupportedNative(pub String);
impl fmt::Display for UnsupportedNative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Native DEX engine does not yet support {}", self.0)
    }
}
impl std::error::Error for UnsupportedNative {}
fn unsupported<T>(reason: impl Into<String>) -> Result<T> {
    Err(UnsupportedNative(reason.into()).into())
}

#[derive(Default)]
pub struct NativeDexEngine {
    classes: BTreeMap<String, DexClass>,
    opened: bool,
}
impl NativeDexEngine {
    pub fn render(&self, name: &str) -> Result<DecompiledCode> {
        let class = self
            .classes
            .get(name)
            .with_context(|| format!("Unknown class: {name}"))?;
        self.render_class(name, class)
    }
    pub fn render_cancellable(
        &self,
        name: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<DecompiledCode> {
        let class = self
            .classes
            .get(name)
            .with_context(|| format!("Unknown class: {name}"))?;
        crate::native_java::render_mixed_cancellable(name, class, cancelled)
    }
    fn render_class(&self, name: &str, class: &DexClass) -> Result<DecompiledCode> {
        Ok(crate::native_java::render_mixed(name, class))
    }

    pub fn definition_names(&self, names: &[String]) -> Result<DefinitionNameIndex> {
        let mut entries = Vec::new();
        for name in names {
            let class = self
                .classes
                .get(name)
                .with_context(|| format!("Unknown class: {name}"))?;
            entries.push(DefinitionName {
                class: name.clone(),
                kind: "class".into(),
                name: name.clone(),
            });
            entries.extend(class.fields.iter().map(|f| DefinitionName {
                class: name.clone(),
                kind: "field".into(),
                name: f.name.to_string(),
            }));
            entries.extend(class.methods.iter().map(|m| DefinitionName {
                class: name.clone(),
                kind: "method".into(),
                name: m.name.to_string(),
            }));
        }
        Ok(DefinitionNameIndex {
            entries,
            unindexed_classes: Vec::new(),
        })
    }
    pub fn class(&self, name: &str) -> Option<&DexClass> {
        self.classes.get(name)
    }
    /// Immediate superclass edges only, including references to external parents.
    /// Uses original DEX identity, independently of source reconstruction/aliases.
    pub fn direct_subclasses(&self, name: &str) -> Vec<String> {
        let descriptor = format!("L{};", name.replace('.', "/"));
        self.classes
            .iter()
            .filter(|(_, class)| {
                class.access_flags & 0x200 == 0
                    && class.superclass.as_deref() == Some(descriptor.as_str())
                    && class.descriptor.as_ref() != descriptor
            })
            .map(|(name, _)| name.clone())
            .collect()
    }
}

fn dex_entry(name: &str) -> bool {
    if name == "classes.dex" {
        return true;
    }
    name.strip_prefix("classes")
        .and_then(|s| s.strip_suffix(".dex"))
        .is_some_and(|s| !s.starts_with('0') && s.parse::<u32>().is_ok_and(|n| n >= 2))
}
fn read_bounded(mut reader: impl Read, remaining: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(remaining + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= remaining,
        "Native DEX input exceeds the 256 MiB aggregate limit"
    );
    Ok(bytes)
}
// Bound the ZIP directory before the ZIP library allocates its entry table.
fn check_zip_directory(file: &mut File) -> Result<()> {
    let size = file.metadata()?.len();
    let tail_size = size.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0; tail_size];
    file.read_exact(&mut tail)?;
    let position = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..i + 4] == *b"PK\x05\x06"
                && i + 22 + usize::from(u16::from_le_bytes([tail[i + 20], tail[i + 21]]))
                    == tail.len()
        })
        .context("Missing standard ZIP end record")?;
    let short = |i: usize| u16::from_le_bytes([tail[position + i], tail[position + i + 1]]);
    let word = |i: usize| {
        u32::from_le_bytes(
            tail[position + i..position + i + 4]
                .try_into()
                .expect("end record bounds"),
        )
    };
    ensure!(
        short(4) == 0 && short(6) == 0 && short(8) == short(10),
        "Split APK ZIP archives are unsupported"
    );
    ensure!(
        short(10) != u16::MAX && word(12) != u32::MAX && word(16) != u32::MAX,
        "ZIP64 APK metadata is not supported by the native backend yet"
    );
    ensure!(
        usize::from(short(10)) <= MAX_ZIP_ENTRIES,
        "APK has too many entries"
    );
    ensure!(
        u64::from(word(12)) <= MAX_ARCHIVE_METADATA,
        "APK central directory exceeds native metadata budget"
    );
    let end_record = size - tail_size as u64 + position as u64;
    ensure!(
        u64::from(word(16)) + u64::from(word(12)) <= end_record,
        "Invalid APK central directory range"
    );
    file.rewind()?;
    Ok(())
}
impl DecompilerEngine for NativeDexEngine {
    fn open(&mut self, path: &Path) -> Result<Project> {
        // Clear stale state even if the replacement input fails.
        self.classes.clear();
        self.opened = false;
        let mut classes = BTreeMap::new();
        let mut resources = Vec::new();
        let mut parse_budget = MAX_RETAINED_DEX_BYTES;
        let mut add_dex = |bytes: &[u8]| -> Result<()> {
            let dex = native_dex::parse_with_budget(bytes, &mut parse_budget).map_err(|error| {
                if error.downcast_ref::<native_dex::UnsupportedDex>().is_some() {
                    anyhow::Error::new(UnsupportedNative(error.to_string()))
                } else {
                    error
                }
            })?;
            for class in dex.classes {
                let name = class
                    .descriptor
                    .strip_prefix('L')
                    .and_then(|s| s.strip_suffix(';'))
                    .context("Invalid class descriptor")?
                    .replace('/', ".");
                ensure!(
                    classes.len() < MAX_CLASSES,
                    "Native class inventory exceeds limit"
                );
                ensure!(
                    !classes.contains_key(&name),
                    "Duplicate class across DEX inputs: {name}"
                );
                classes.insert(name, class);
            }
            Ok(())
        };
        let mut file = File::open(path)?;
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("dex") => {
                ensure!(
                    file.metadata()?.len() <= MAX_DEX_BYTES,
                    "Native DEX input exceeds size limit"
                );
                add_dex(&read_bounded(file, MAX_DEX_BYTES)?)?;
            }
            Some("apk") => {
                check_zip_directory(&mut file)?;
                let mut archive = zip::ZipArchive::new(file)?;
                ensure!(archive.len() <= MAX_ZIP_ENTRIES, "APK has too many entries");
                let mut remaining = MAX_DEX_BYTES;
                let mut dex_count = 0;
                let mut resource_name_bytes = 0;
                for i in 0..archive.len() {
                    let mut entry = archive.by_index(i)?;
                    ensure!(entry.name().len() <= 4096, "APK entry name exceeds limit");
                    resource_name_bytes += entry.name().len() as u64 + 24;
                    ensure!(
                        resource_name_bytes <= MAX_ARCHIVE_METADATA,
                        "APK resource inventory exceeds native metadata budget"
                    );
                    resources.push(entry.name().to_owned());
                    if !dex_entry(entry.name()) {
                        continue;
                    }
                    ensure!(entry.size() <= remaining, "APK DEX data exceeds size limit");
                    let bytes = read_bounded(&mut entry, remaining)?;
                    remaining -= bytes.len() as u64;
                    add_dex(&bytes).with_context(|| format!("Parsing {}", entry.name()))?;
                    dex_count += 1;
                }
                ensure!(
                    dex_count > 0,
                    "APK contains no standard classes*.dex entries"
                );
            }
            _ => bail!("Choose an APK or DEX file"),
        }
        ensure!(!classes.is_empty(), "No classes in DEX input");
        let hierarchy = std::sync::Arc::new(crate::native_hierarchy::TypeHierarchy::from_classes(
            classes.values(),
        )?);
        for class in classes.values() {
            // Symbols are shared per DEX. All DEX files use the same immutable
            // project hierarchy, so cross-DEX ancestry is available to rendering.
            class
                .symbols
                .hierarchy
                .get_or_init(|| std::sync::Arc::clone(&hierarchy));
        }
        let project = Project {
            classes: classes.keys().cloned().collect(),
            resources,
        };
        self.classes = classes;
        self.opened = true;
        Ok(project)
    }
    fn decompile(&mut self, name: &str) -> Result<String> {
        ensure!(self.opened, "Open an APK or DEX first");
        let class = self
            .classes
            .get(name)
            .with_context(|| format!("Unknown class: {name}"))?;
        Ok(self.render_class(name, class)?.source)
    }
    fn read_resource(&mut self, _path: &str) -> Result<String> {
        ensure!(self.opened, "Open an APK or DEX first");
        unsupported("decoded Android resources")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_subclasses_use_exact_immediate_superclass_metadata() {
        let mut engine = NativeDexEngine::default();
        for (name, parent, interface) in [
            ("sample.Child", "sample.Parent", false),
            ("sample.Grandchild", "sample.Child", false),
            ("sample.Other", "other.Parent", false),
            ("sample.Implementer", "java.lang.Object", false),
            ("sample.Contract", "java.lang.Object", true),
            ("sample.ΔChild", "sample.Parent", false),
        ] {
            let mut class = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
                .unwrap()
                .classes
                .remove(0);
            class.descriptor = format!("L{};", name.replace('.', "/")).into();
            class.superclass = Some(format!("L{};", parent.replace('.', "/")).into());
            class.interfaces = vec!["Lsample/Parent;".into()];
            class.access_flags = if interface { 0x601 } else { 1 };
            engine.classes.insert(name.into(), class);
        }
        // Parent need not be defined in the APK; all DEX files share this catalog.
        assert_eq!(
            engine.direct_subclasses("sample.Parent"),
            ["sample.Child", "sample.ΔChild"]
        );
        assert_eq!(
            engine.direct_subclasses("java.lang.Object"),
            ["sample.Implementer"]
        );
        assert!(engine.direct_subclasses("sample.Missing").is_empty());
    }
    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
    #[test]
    fn opens_and_reconstructs_without_java() {
        for file in ["hello.dex", "hello.apk"] {
            let mut engine = NativeDexEngine::default();
            assert_eq!(
                engine.open(&fixture(file)).unwrap().classes,
                ["sample.Hello"]
            );
            let source = engine.decompile("sample.Hello").unwrap();
            assert!(source.contains("public static int answer()"));
            assert!(source.contains("return 42;"));
            assert!(
                engine
                    .decompile("sample.Missing")
                    .unwrap_err()
                    .downcast_ref::<UnsupportedNative>()
                    .is_none()
            );
        }
    }
    #[test]
    fn navigation_fixture_keeps_native_declarations() {
        let mut engine = NativeDexEngine::default();
        engine.open(&fixture("navigation.dex")).unwrap();
        let code = engine.render("sample.Target").unwrap();
        assert!(code.source.contains("doubleValue"));
        assert!(!code.definitions.is_empty());
        assert!(engine.open(&fixture("missing.dex")).is_err());
        assert!(engine.class("sample.Target").is_none());
    }
    #[test]
    fn constant_widths_and_signs_are_preserved() {
        for (instructions, expected) in [
            (vec![0xf012, 0x000f], -1),
            (vec![0x0013, 0x8000, 0x000f], -32768),
            (vec![0x0014, 0xffff, 0x7fff, 0x000f], i32::MAX),
            (vec![0x0015, 0x8000, 0x000f], i32::MIN),
        ] {
            let mut dex = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
            let method = &mut dex.classes[0].methods[0];
            method.code.as_mut().unwrap().instructions = instructions;
            assert!(
                crate::native_java::render("sample.Hello", &dex.classes[0])
                    .unwrap()
                    .source
                    .contains(&format!("return {expected};"))
            );
        }
    }
    #[test]
    fn unsupported_instructions_are_not_rendered_as_fake_source() {
        for instructions in [
            vec![0x0028],
            vec![0x0013, 42, 0x010f],
            vec![0x0013, 42, 0x000f, 0x0000],
        ] {
            let mut dex = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
            dex.classes[0].methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions = instructions;
            assert!(crate::native_java::render("sample.Hello", &dex.classes[0]).is_err());
        }
    }

    #[test]
    #[ignore = "Set RDX_EXPEDIA_APK to the pinned Expedia APK and run --ignored"]
    fn expedia_multidex_inventory_fits_bounded_retained_budget() {
        let input = std::env::var_os("RDX_EXPEDIA_APK").expect("RDX_EXPEDIA_APK required");
        let mut engine = NativeDexEngine::default();
        let project = engine.open(Path::new(&input)).unwrap();
        assert_eq!(project.classes.len(), 228_363);
        assert_eq!(
            project
                .resources
                .iter()
                .filter(|name| dex_entry(name))
                .count(),
            25
        );
    }
}
