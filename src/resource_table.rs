//! Native resources.arsc index. Layout follows AOSP androidfw ResourceTypes.h.
use crate::{
    engine::{CodeLink, DecompiledCode, source_identity},
    native_resources::{escape, pool, typed, u16_at, u32_at},
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

pub const PREFIX: &str = "resource://";
const MAX_VALUES: usize = 2_000_000;
#[derive(Clone, Debug)]
pub struct Variant {
    pub configuration: String,
    pub value: String,
    pub file: Option<String>,
    pub reference: Option<u32>,
}
#[derive(Debug)]
pub struct Resource {
    pub id: u32,
    pub package: String,
    pub kind: String,
    pub name: String,
    pub variants: Vec<Variant>,
}
#[derive(Default)]
pub struct ResourceTable {
    pub entries: BTreeMap<u32, Resource>,
}
fn chunk(b: &[u8], p: usize) -> Result<(u16, usize, &[u8])> {
    let kind = u16_at(b, p)?;
    let header = u16_at(b, p + 2)? as usize;
    let size = u32_at(b, p + 4)? as usize;
    ensure!(header >= 8 && size >= header, "Invalid resource chunk size");
    let end = p.checked_add(size).context("Resource chunk overflow")?;
    Ok((
        kind,
        header,
        b.get(p..end).context("Truncated resource chunk")?,
    ))
}
fn strings(b: &[u8], offset: usize) -> Result<Vec<String>> {
    let (kind, header, data) = chunk(b, offset)?;
    ensure!(kind == 1, "Expected resource string pool");
    pool(data, header)
}
fn config(b: &[u8]) -> String {
    if b.iter().all(|x| *x == 0) {
        return "default".into();
    }
    // Preserve every qualifier byte; a resource viewer is not a device matcher.
    let locale = b.get(4..8).unwrap_or(&[]);
    let locale = locale
        .iter()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| *c as char)
        .collect::<String>();
    let hex = b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    if locale.is_empty() {
        format!("config-{hex}")
    } else {
        format!("{locale} / config-{hex}")
    }
}
impl ResourceTable {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "Resource table exceeds 64 MiB"
        );
        let (kind, header, b) = chunk(bytes, 0)?;
        ensure!(
            kind == 2 && header >= 12 && b.len() == bytes.len(),
            "Invalid resource table"
        );
        let mut global = Vec::new();
        let mut packages = Vec::new();
        let mut p = header;
        while p < b.len() {
            let (kind, h, c) = chunk(b, p)?;
            match kind {
                1 => global = pool(c, h)?,
                0x200 => packages.push((h, c)),
                _ => {}
            }
            p += c.len();
        }
        ensure!(
            packages.len() == u32_at(b, 8)? as usize,
            "Resource package count mismatch"
        );
        let mut table = Self::default();
        let mut count = 0usize;
        let mut retained = 0usize;
        for (header, b) in packages {
            ensure!(header >= 284, "Invalid resource package header");
            let package_id = u32_at(b, 8)?;
            ensure!(
                (1..=255).contains(&package_id),
                "Unassigned resource package ID"
            );
            let name = (12..268)
                .step_by(2)
                .map(|p| u16_at(b, p))
                .collect::<Result<Vec<_>>>()?;
            let end = name.iter().position(|x| *x == 0).unwrap_or(name.len());
            let package = String::from_utf16(&name[..end])?;
            let types = strings(b, u32_at(b, 268)? as usize)?;
            let keys = strings(b, u32_at(b, 276)? as usize)?;
            let type_offset = if header >= 288 { u32_at(b, 284)? } else { 0 };
            let mut p = header;
            while p < b.len() {
                let (kind, h, c) = chunk(b, p)?;
                p += c.len();
                if kind != 0x201 {
                    continue;
                }
                ensure!(h >= 24, "Invalid resource type header");
                let type_id = c[8] as u32;
                ensure!(
                    type_id > 0 && type_id + type_offset <= 255,
                    "Invalid resource type ID"
                );
                let kind = types
                    .get(type_id as usize - 1)
                    .context("Resource type name missing")?;
                let flags = c[9];
                ensure!(
                    flags & !3 == 0 && flags != 3,
                    "Unsupported resource offset flags"
                );
                let n = u32_at(c, 12)? as usize;
                let start = u32_at(c, 16)? as usize;
                let cfgsize = u32_at(c, 20)? as usize;
                ensure!(
                    cfgsize >= 4 && 20 + cfgsize <= h,
                    "Invalid resource configuration"
                );
                let configuration = config(&c[24..20 + cfgsize]);
                let width = if flags & 2 != 0 { 2 } else { 4 };
                ensure!(
                    n <= 65536 && h + n * width <= start && start <= c.len(),
                    "Invalid resource entry offsets"
                );
                for i in 0..n {
                    let (index, off) = if flags & 1 != 0 {
                        (
                            u16_at(c, h + i * 4)? as u32,
                            u16_at(c, h + i * 4 + 2)? as u32 * 4,
                        )
                    } else if flags & 2 != 0 {
                        let off = u16_at(c, h + i * 2)?;
                        (
                            i as u32,
                            if off == 0xffff {
                                u32::MAX
                            } else {
                                off as u32 * 4
                            },
                        )
                    } else {
                        (i as u32, u32_at(c, h + i * 4)?)
                    };
                    if off == u32::MAX {
                        continue;
                    }
                    count += 1;
                    ensure!(count <= MAX_VALUES, "Resource variant budget exceeded");
                    let e = start
                        .checked_add(off as usize)
                        .context("Entry offset overflow")?;
                    let size = u16_at(c, e)? as usize;
                    let flags = u16_at(c, e + 2)?;
                    let compact = flags & 8 != 0;
                    ensure!(!compact || flags & 1 == 0, "Complex compact resource entry");
                    let key = if compact {
                        size
                    } else {
                        u32_at(c, e + 4)? as usize
                    };
                    let name = keys.get(key).context("Resource key missing")?;
                    let id = (package_id << 24) | ((type_id + type_offset) << 16) | index;
                    let (value, file, reference) = if flags & 1 != 0 {
                        ensure!(size >= 16 && e + size <= c.len(), "Invalid complex entry");
                        let parent = u32_at(c, e + 8)?;
                        let maps = u32_at(c, e + 12)? as usize;
                        ensure!(
                            maps <= 65536 && e + size + maps * 12 <= c.len(),
                            "Invalid resource map"
                        );
                        let mut v = format!("parent=@0x{parent:08x}");
                        for j in 0..maps {
                            let a = e + size + j * 12;
                            ensure!(u16_at(c, a + 4)? == 8, "Invalid map value size");
                            v.push_str(&format!(
                                "\n@0x{:08x} = {}",
                                u32_at(c, a)?,
                                typed(&global, c[a + 7], u32_at(c, a + 8)?)?
                            ));
                        }
                        (v, None, None)
                    } else {
                        let (ty, data) = if compact {
                            ((flags >> 8) as u8, u32_at(c, e + 4)?)
                        } else {
                            ensure!(
                                size >= 8 && u16_at(c, e + size)? == 8,
                                "Invalid resource value size"
                            );
                            (
                                *c.get(e + size + 3).context("Missing resource value type")?,
                                u32_at(c, e + size + 4)?,
                            )
                        };
                        let value = typed(&global, ty, data)?;
                        let file = (ty == 3 && kind != "string" && value.starts_with("res/"))
                            .then(|| value.clone());
                        (value, file, matches!(ty, 1 | 2).then_some(data))
                    };
                    retained += value.len()
                        + configuration.len()
                        + name.len()
                        + package.len()
                        + kind.len()
                        + 128;
                    ensure!(
                        retained <= 256 * 1024 * 1024,
                        "Decoded resource index exceeds budget"
                    );
                    let resource = table.entries.entry(id).or_insert_with(|| Resource {
                        id,
                        package: package.clone(),
                        kind: kind.clone(),
                        name: name.clone(),
                        variants: vec![],
                    });
                    ensure!(
                        resource.name == *name
                            && resource.kind == *kind
                            && resource.package == package,
                        "Ambiguous resource ID"
                    );
                    resource.variants.push(Variant {
                        configuration: configuration.clone(),
                        value,
                        file,
                        reference,
                    });
                }
            }
        }
        for r in table.entries.values_mut() {
            r.variants.sort_by(|a, b| {
                (a.configuration != "default", &a.configuration)
                    .cmp(&(b.configuration != "default", &b.configuration))
            });
        }
        Ok(table)
    }
    pub fn label(&self, id: u32) -> Option<String> {
        let r = self.entries.get(&id)?;
        let first = r.variants.first()?;
        let preview = first
            .value
            .chars()
            .take(160)
            .collect::<String>()
            .replace(['\n', '\r'], " ");
        Some(format!(
            "{PREFIX}{id:08x}/{}/{} | {}:{}.{} = {} [{}; {} variant(s)]",
            r.kind,
            r.name,
            r.package,
            r.kind,
            r.name,
            preview,
            first.configuration,
            r.variants.len()
        ))
    }
    pub fn document(&self, id: u32) -> Result<DecompiledCode> {
        let r = self
            .entries
            .get(&id)
            .context("Resource ID absent from loaded APK")?;
        let mut s = format!(
            "<!-- {}:{}/{} · 0x{id:08x} -->\n<resources>\n",
            escape(&r.package),
            escape(&r.kind),
            escape(&r.name)
        );
        let mut links = Vec::new();
        let mut references = Vec::new();
        let mut position = s.chars().count();
        for v in &r.variants {
            let prefix = format!(
                "  <item type=\"{}\" name=\"{}\" configuration=\"{}\">",
                escape(&r.kind),
                escape(&r.name),
                escape(&v.configuration)
            );
            position += prefix.chars().count();
            s.push_str(&prefix);
            let start = position;
            let value = escape(&v.value);
            position += value.chars().count();
            s.push_str(&value);
            if v.reference.is_some() {
                references.push(start..position);
            }
            if let Some(file) = &v.file {
                links.push(CodeLink {
                    start,
                    end: position,
                    label: format!("{PREFIX}file/{file}"),
                });
            }
            s.push_str("</item>\n");
            position += 8;
            ensure!(
                s.len() <= 16 * 1024 * 1024,
                "Resource detail exceeds 16 MiB"
            );
        }
        s.push_str("</resources>\n");
        let mut code = DecompiledCode {
            source_hash: source_identity(&s),
            source: s,
            links,
            definitions: vec![],
        };
        self.decorate_xml(&mut code, &references);
        Ok(code)
    }
    /// Link/annotate numeric IDs without changing integer semantics. Java tokens
    /// inside strings/comments and metadata spans are never rewritten.
    pub fn decorate(&self, code: &mut DecompiledCode, xml: bool) {
        self.decorate_inner(code, xml, None);
    }
    pub fn decorate_xml(&self, code: &mut DecompiledCode, references: &[std::ops::Range<usize>]) {
        self.decorate_inner(code, true, Some(references));
    }
    fn decorate_inner(
        &self,
        code: &mut DecompiledCode,
        xml: bool,
        references: Option<&[std::ops::Range<usize>]>,
    ) {
        if self.entries.is_empty() {
            return;
        }
        let chars: Vec<char> = code.source.chars().collect();
        let mut edits = Vec::new();
        let mut occupied: Vec<_> = code
            .links
            .iter()
            .map(|l| (l.start, l.end))
            .chain(code.definitions.iter().map(|d| (d.start, d.end)))
            .collect();
        occupied.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in occupied {
            if let Some(last) = merged.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        let mut i = 0;
        while i < chars.len() {
            if !xml && (chars[i] == '"' || chars[i] == '\'') {
                let quote = chars[i];
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i = (i + 2).min(chars.len());
                    } else if chars[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            if !xml && chars[i] == '/' && i + 1 < chars.len() && matches!(chars[i + 1], '/' | '*') {
                let block = chars[i + 1] == '*';
                i += 2;
                while i < chars.len() {
                    if !block && chars[i] == '\n' {
                        break;
                    }
                    if block && i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            let start = i;
            let prefix = xml
                && matches!(chars[i], '@' | '?')
                && chars.get(i + 1) == Some(&'0')
                && chars.get(i + 2) == Some(&'x');
            if prefix {
                i += 1;
            }
            if (xml && !prefix)
                || !chars[i].is_ascii_digit()
                || (!prefix
                    && start > 0
                    && (chars[start - 1].is_alphanumeric()
                        || matches!(chars[start - 1], '_' | '.' | '$')))
            {
                i = start + 1;
                continue;
            }
            let number_start = i;
            let hex = chars[i] == '0' && chars.get(i + 1) == Some(&'x');
            if hex {
                i += 2;
            }
            while i < chars.len()
                && if hex {
                    chars[i].is_ascii_hexdigit()
                } else {
                    chars[i].is_ascii_digit()
                }
            {
                i += 1;
            }
            if i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.' | '$'))
            {
                continue;
            }
            if let Some(ranges) = references {
                let n = ranges.partition_point(|range| range.end <= start);
                if !ranges
                    .get(n)
                    .is_some_and(|range| range.start <= start && i <= range.end)
                {
                    continue;
                }
            }
            let raw = chars[number_start..i].iter().collect::<String>();
            let id = if hex {
                u32::from_str_radix(&raw[2..], 16)
            } else {
                raw.parse::<u32>()
            };
            let Ok(id) = id else {
                continue;
            };
            let Some(r) = self.entries.get(&id) else {
                continue;
            };
            let n = merged.partition_point(|(_, end)| *end <= start);
            if merged.get(n).is_some_and(|(begin, _)| *begin < i) {
                continue;
            }
            let name = format!("{}.R.{}.{}", r.package, r.kind, r.name)
                .chars()
                .take(200)
                .collect::<String>()
                .replace('\\', "\\\\")
                .replace("*/", "* /")
                .replace(['\n', '\r'], " ");
            // Keep original numeric value visible, avoiding invented R classes or
            // accidental changes to unrelated integer expressions with the same ID.
            let replacement = if xml {
                format!(
                    "{}{}:{}/{}",
                    chars[start],
                    escape(&r.package),
                    escape(&r.kind),
                    escape(&r.name)
                )
            } else {
                let value = r
                    .variants
                    .first()
                    .filter(|v| v.configuration == "default")
                    .map(|v| {
                        let preview = v.value.chars().take(80).collect::<String>();
                        let quoted = serde_json::to_string(&preview)
                            .unwrap_or_default()
                            .replace("*/", "* /");
                        format!(
                            " = {quoted}{}",
                            if v.value.chars().count() > 80 {
                                "…"
                            } else {
                                ""
                            }
                        )
                    })
                    .unwrap_or_default();
                format!("{raw} /* {name}{value} */")
            };
            edits.push((start, i, replacement, self.label(id).unwrap()));
        }
        if edits.is_empty() {
            return;
        }
        let mut source = String::new();
        let mut last = 0;
        let mut added = Vec::new();
        let mut shifts = Vec::new();
        let mut delta = 0isize;
        for (start, end, text, label) in &edits {
            source.extend(&chars[last..*start]);
            let at = (*start as isize + delta) as usize;
            source.push_str(text);
            added.push(CodeLink {
                start: at,
                end: at + text.chars().count(),
                label: label.clone(),
            });
            delta += text.chars().count() as isize - (*end - *start) as isize;
            shifts.push((*end, delta));
            last = *end;
        }
        source.extend(&chars[last..]);
        let remap = |p: usize| {
            let n = shifts.partition_point(|(end, _)| *end <= p);
            (p as isize + if n == 0 { 0 } else { shifts[n - 1].1 }) as usize
        };
        for l in &mut code.links {
            l.start = remap(l.start);
            l.end = remap(l.end);
        }
        for d in &mut code.definitions {
            d.start = remap(d.start);
            d.end = remap(d.end);
        }
        code.links.extend(added);
        code.links.sort_by_key(|l| l.start);
        code.source = source;
        code.source_hash = source_identity(&code.source);
    }
}
