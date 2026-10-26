//! Native Android binary XML decoding. See third_party/jadx/README.md for reference mapping.
use anyhow::{Context, Result, bail, ensure};
#[path = "native_resource_attributes.rs"]
mod attributes;
#[path = "native_resource_names.rs"]
mod resource_names;
const NONE: u32 = u32::MAX;
const ANDROID_NAMESPACE: &str = "http://schemas.android.com/apk/res/android";
/// Shared cap for XML archive reads and native decoding.
pub const MAX_XML_BYTES: usize = 32 * 1024 * 1024;
const LIMIT: usize = MAX_XML_BYTES;
pub(crate) fn u16_at(b: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2)
            .context("Truncated binary XML")?
            .try_into()?,
    ))
}
pub(crate) fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .context("Truncated binary XML")?
            .try_into()?,
    ))
}
fn string(strings: &[String], id: u32) -> Result<&str> {
    strings
        .get(id as usize)
        .map(String::as_str)
        .context("Invalid XML string index")
}
fn length(b: &[u8], p: &mut usize, wide: bool) -> Result<usize> {
    let (unit, high) = if wide { (2, 0x8000) } else { (1, 0x80) };
    let mut n = if wide {
        u16_at(b, *p)? as usize
    } else {
        *b.get(*p).context("Truncated string length")? as usize
    };
    *p += unit;
    if n & high != 0 {
        let lo = if wide {
            u16_at(b, *p)? as usize
        } else {
            *b.get(*p).context("Truncated string length")? as usize
        };
        *p += unit;
        n = ((n & !high) << (unit * 8)) | lo;
    }
    Ok(n)
}
// Some aapt tables encode supplementary characters as CESU-8 surrogate pairs.
// Accept those pairs without accepting malformed continuations or overlong UTF-8.
fn resource_utf8(bytes: &[u8]) -> Result<String> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(s.to_owned());
    }
    let mut units = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let first = bytes[i];
        let n = match first {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => bail!("Invalid resource UTF-8 lead byte"),
        };
        let part = bytes.get(i..i + n).context("Truncated resource UTF-8")?;
        if n == 3 {
            ensure!(
                part[1] & 0xc0 == 0x80 && part[2] & 0xc0 == 0x80,
                "Invalid resource UTF-8 continuation"
            );
            let unit =
                ((first as u16 & 15) << 12) | ((part[1] as u16 & 63) << 6) | (part[2] as u16 & 63);
            ensure!(unit >= 0x800, "Overlong resource UTF-8");
            units.push(unit);
        } else {
            units.extend(std::str::from_utf8(part)?.encode_utf16());
        }
        i += n;
    }
    Ok(String::from_utf16(&units)?)
}
pub(crate) fn pool(b: &[u8], header: usize) -> Result<Vec<String>> {
    ensure!(header >= 28, "Invalid string pool header");
    let count = u32_at(b, 8)? as usize;
    let styles = u32_at(b, 12)? as usize;
    let utf8 = u32_at(b, 16)? & 0x100 != 0;
    let start = u32_at(b, 20)? as usize;
    let style_start = u32_at(b, 24)? as usize;
    let end = if style_start == 0 {
        b.len()
    } else {
        style_start
    };
    ensure!(
        count <= 1_000_000 && styles <= 1_000_000,
        "XML string pool too large"
    );
    ensure!(
        header + (count + styles) * 4 <= start && start <= end && end <= b.len(),
        "Invalid string pool bounds"
    );
    let b = &b[..end];
    let mut output = Vec::with_capacity(count);
    let mut decoded_bytes = 0;
    for i in 0..count {
        let mut p = start
            .checked_add(u32_at(b, header + i * 4)? as usize)
            .context("String offset overflow")?;
        let units = length(b, &mut p, !utf8)?;
        let value = if utf8 {
            let bytes = length(b, &mut p, false)?;
            let end = p.checked_add(bytes).context("String size overflow")?;
            ensure!(b.get(end) == Some(&0), "Missing XML string terminator");
            let s = resource_utf8(b.get(p..end).context("Invalid UTF8 string bounds")?)?;
            ensure!(
                s.encode_utf16().count() == units,
                "Invalid XML string length"
            );
            s
        } else {
            let end = p
                .checked_add(units.checked_mul(2).context("String size overflow")?)
                .context("String size overflow")?;
            ensure!(u16_at(b, end)? == 0, "Missing XML string terminator");
            let raw = b.get(p..end).context("Invalid UTF16 string bounds")?;
            String::from_utf16(
                &raw.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>(),
            )?
        };
        decoded_bytes += value.len();
        ensure!(
            decoded_bytes <= LIMIT,
            "Decoded XML string pool exceeds 32 MiB"
        );
        output.push(value);
    }
    Ok(output)
}
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#9;")
}
fn name(s: &str) -> Result<&str> {
    ensure!(
        !s.is_empty()
            && s.chars().enumerate().all(|(i, c)| c == '_'
                || c.is_alphabetic()
                || (i > 0 && (c.is_ascii_digit() || c == '-' || c == '.'))),
        "Invalid XML name"
    );
    Ok(s)
}
fn qualified(strings: &[String], namespaces: &[(u32, u32)], ns: u32, id: u32) -> Result<String> {
    let local = name(string(strings, id)?)?;
    if ns == NONE {
        return Ok(local.to_owned());
    }
    let prefix = namespaces
        .iter()
        .rev()
        .find(|(_, uri)| *uri == ns)
        .context("Undeclared XML namespace")?
        .0;
    let prefix = string(strings, prefix)?;
    if prefix.is_empty() {
        Ok(local.to_owned())
    } else {
        Ok(format!("{}:{local}", name(prefix)?))
    }
}
// Resource IDs survive attribute-name and namespace stripping by APK optimizers.
// Use an active, non-default binding: default namespaces do not qualify attributes.
fn namespace_prefix(
    strings: &[String],
    namespaces: &[(u32, u32)],
    wanted: &str,
    base: &str,
) -> Result<(String, bool)> {
    let mut seen = std::collections::HashSet::new();
    for &(prefix, uri) in namespaces.iter().rev() {
        let prefix = string(strings, prefix)?;
        if seen.insert(prefix) && !prefix.is_empty() && string(strings, uri)? == wanted {
            return Ok((name(prefix)?.to_owned(), false));
        }
    }
    let mut prefix = String::from(base);
    let mut suffix = 0;
    while seen.contains(prefix.as_str()) {
        suffix += 1;
        prefix = format!("{base}{suffix}");
    }
    Ok((prefix, true))
}
pub(crate) fn typed(strings: &[String], ty: u8, data: u32) -> Result<String> {
    Ok(match ty {
        0 => if data == 1 { "@empty" } else { "@null" }.to_owned(),
        1 | 7 => format!("@0x{data:08x}"),
        2 | 8 => format!("?0x{data:08x}"),
        3 => string(strings, data)?.to_owned(),
        4 => f32::from_bits(data).to_string(),
        5 | 6 => {
            let radix = ((data >> 4) & 3) as usize;
            let value = ((data & 0xffffff00) as i32) as f64
                * [
                    1.0 / 256.0,
                    1.0 / 32768.0,
                    1.0 / 8388608.0,
                    1.0 / 2147483648.0,
                ][radix];
            let unit = (data & 15) as usize;
            if ty == 5 {
                format!(
                    "{value}{}",
                    ["px", "dp", "sp", "pt", "in", "mm"]
                        .get(unit)
                        .context("Invalid dimension unit")?
                )
            } else {
                format!(
                    "{}{}",
                    value * 100.0,
                    ["%", "%p"].get(unit).context("Invalid fraction unit")?
                )
            }
        }
        0x10 => (data as i32).to_string(),
        0x11 => format!("0x{data:x}"),
        0x12 => (data != 0).to_string(),
        0x1c => format!("#{data:08x}"),
        0x1d => format!("#{:06x}", data & 0xffffff),
        0x1e => format!("#{:04x}", data & 0xffff),
        0x1f => format!("#{:03x}", data & 0xfff),
        _ => bail!("Unsupported XML value type 0x{ty:02x}"),
    })
}
/// Decode Android compiled XML without a JVM. Resource references remain numeric IDs.
/// Plain UTF-8 XML is returned unchanged. Input and decoded output are limited to 32 MiB.
pub fn decode(bytes: &[u8]) -> Result<String> {
    Ok(decode_with_references(bytes)?.0)
}
/// Character spans of actual compiled reference attributes, excluding literal strings.
pub fn decode_with_references(bytes: &[u8]) -> Result<(String, Vec<std::ops::Range<usize>>)> {
    decode_xml(bytes, &|_| None, false)
}
/// Recover application attribute names by exact resource ID from the loaded ARSC table.
/// Stripped, unmapped application attributes retain their IDs as resource_0xID names.
/// The default decoder remains independent of an application table.
pub fn decode_with_attribute_resolver<'a>(
    bytes: &[u8],
    attribute_name: &impl Fn(u32) -> Option<&'a str>,
) -> Result<(String, Vec<std::ops::Range<usize>>)> {
    decode_xml(bytes, attribute_name, true)
}
fn decode_xml<'a>(
    bytes: &[u8],
    attribute_name: &impl Fn(u32) -> Option<&'a str>,
    preserve_application_ids: bool,
) -> Result<(String, Vec<std::ops::Range<usize>>)> {
    let mut references = Vec::new();
    ensure!(bytes.len() <= LIMIT, "XML input exceeds 32 MiB");
    if let Ok(text) = std::str::from_utf8(bytes)
        && text
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<')
    {
        return Ok((text.to_owned(), references));
    }
    ensure!(
        u16_at(bytes, 0)? == 3 && u16_at(bytes, 2)? == 8,
        "Not Android binary XML or UTF-8 XML"
    );
    ensure!(
        u32_at(bytes, 4)? as usize == bytes.len(),
        "Invalid XML document length"
    );
    let mut p = 8;
    let mut strings = Vec::new();
    let mut resource_map = None;
    let mut namespaces = Vec::new();
    let mut pending = Vec::new();
    let mut stack = Vec::new();
    let mut roots = 0;
    let mut output = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    while p < bytes.len() {
        let ty = u16_at(bytes, p)?;
        let header = u16_at(bytes, p + 2)? as usize;
        let size = u32_at(bytes, p + 4)? as usize;
        ensure!(
            header >= 8 && size >= header && size <= bytes.len() - p,
            "Invalid XML chunk bounds"
        );
        let b = &bytes[p..p + size];
        match ty {
            1 => {
                ensure!(
                    strings.is_empty() && stack.is_empty() && roots == 0,
                    "Misplaced XML string pool"
                );
                strings = pool(b, header)?;
            }
            0x180 => {
                ensure!(
                    header == 8 && (size - header).is_multiple_of(4),
                    "Invalid XML resource map"
                );
                ensure!(
                    resource_map.is_none() && roots == 0,
                    "Misplaced XML resource map"
                );
                ensure!(
                    (size - header) / 4 <= strings.len(),
                    "XML resource map exceeds string pool"
                );
                resource_map = Some(&b[header..]);
            }
            0x100 | 0x101 => {
                ensure!(header >= 16, "Invalid namespace header");
                let pair = (u32_at(b, header)?, u32_at(b, header + 4)?);
                string(&strings, pair.0)?;
                string(&strings, pair.1)?;
                if ty == 0x100 {
                    namespaces.push(pair);
                    pending.push(pair);
                } else {
                    ensure!(namespaces.pop() == Some(pair), "Unbalanced XML namespaces");
                }
            }
            0x102 => {
                ensure!(header >= 16, "Invalid element header");
                let ns = u32_at(b, header)?;
                let id = u32_at(b, header + 4)?;
                let tag = qualified(&strings, &namespaces, ns, id)?;
                let start = u16_at(b, header + 8)? as usize;
                let stride = u16_at(b, header + 10)? as usize;
                let count = u16_at(b, header + 12)? as usize;
                ensure!(
                    start >= 20 && stride >= 20 && header + start + stride * count <= size,
                    "Invalid XML attributes"
                );
                if stack.is_empty() {
                    roots += 1;
                    ensure!(roots == 1, "Multiple XML roots");
                }
                ensure!(stack.len() < 256, "XML nesting exceeds 256 elements");
                output.push_str(&"    ".repeat(stack.len()));
                output.push('<');
                output.push_str(&tag);
                for (prefix, uri) in pending.drain(..) {
                    let prefix = string(&strings, prefix)?;
                    output.push_str(" xmlns");
                    if !prefix.is_empty() {
                        output.push(':');
                        output.push_str(name(prefix)?);
                    }
                    output.push_str("=\"");
                    output.push_str(&escape(string(&strings, uri)?));
                    output.push('"');
                }
                let mut attributes = std::collections::HashSet::new();
                let mut recovered_prefix = None;
                let mut recovered_app_prefix = None;
                for i in 0..count {
                    let a = header + start + i * stride;
                    let namespace = u32_at(b, a)?;
                    let name_id = u32_at(b, a + 4)?;
                    let original_name = string(&strings, name_id)?;
                    let resource_id = resource_map
                        .and_then(|map| map.get(name_id as usize * 4..name_id as usize * 4 + 4))
                        .and_then(|id| Some(u32::from_le_bytes(id.try_into().ok()?)));
                    let mapped_name = resource_id.and_then(resource_names::attribute);
                    let application_name = resource_id.and_then(attribute_name);
                    // Optimizers can replace ARSC attribute names with their decimal ID.
                    // Preserve that exact identity without claiming a recovered semantic name.
                    let identity_name;
                    let (key, local, uri) = if let Some(local) = mapped_name {
                        let prefix = match &recovered_prefix {
                            Some(prefix) => prefix,
                            None => {
                                let (prefix, declare) = namespace_prefix(
                                    &strings,
                                    &namespaces,
                                    ANDROID_NAMESPACE,
                                    "android",
                                )?;
                                if declare {
                                    output.push_str(&format!(
                                        " xmlns:{prefix}=\"{ANDROID_NAMESPACE}\""
                                    ));
                                }
                                recovered_prefix.insert(prefix)
                            }
                        };
                        (format!("{prefix}:{local}"), local, ANDROID_NAMESPACE)
                    } else if let Some(id) = resource_id.filter(|id| {
                        application_name.is_some()
                            || (preserve_application_ids
                                && original_name.is_empty()
                                && (2..=254).contains(&(id >> 24))
                                && id & 0x00ff0000 != 0)
                    }) {
                        let local = application_name.unwrap_or("");
                        let local = if local.is_empty() || local == id.to_string() {
                            identity_name = format!("resource_0x{id:08x}");
                            identity_name.as_str()
                        } else {
                            local
                        };
                        let local = name(local)?;
                        const APP_NAMESPACE: &str = "http://schemas.android.com/apk/res-auto";
                        let prefix = match &recovered_app_prefix {
                            Some(prefix) => prefix,
                            None => {
                                let (prefix, declare) =
                                    namespace_prefix(&strings, &namespaces, APP_NAMESPACE, "app")?;
                                if declare {
                                    output
                                        .push_str(&format!(" xmlns:{prefix}=\"{APP_NAMESPACE}\""));
                                }
                                recovered_app_prefix.insert(prefix)
                            }
                        };
                        (format!("{prefix}:{local}"), local, APP_NAMESPACE)
                    } else {
                        (
                            qualified(&strings, &namespaces, namespace, name_id)?,
                            original_name,
                            if namespace == NONE {
                                ""
                            } else {
                                string(&strings, namespace)?
                            },
                        )
                    };
                    ensure!(
                        attributes.insert((uri.to_owned(), local.to_owned())),
                        "Duplicate XML attribute"
                    );
                    let raw = u32_at(b, a + 8)?;
                    ensure!(u16_at(b, a + 12)? == 8, "Invalid XML typed value");
                    let value = if raw != NONE {
                        string(&strings, raw)?.to_owned()
                    } else {
                        let symbolic =
                            attributes::symbolic(uri, local, b[a + 15], u32_at(b, a + 16)?);
                        match symbolic {
                            Some(value) => value,
                            None => typed(&strings, b[a + 15], u32_at(b, a + 16)?)?,
                        }
                    };
                    output.push(' ');
                    output.push_str(&key);
                    output.push_str("=\"");
                    let start = output.len();
                    output.push_str(&escape(&value));
                    if matches!(b[a + 15], 1 | 2) && raw == NONE {
                        references.push(start..output.len());
                    }
                    output.push('"');
                    ensure!(output.len() <= LIMIT, "Decoded XML exceeds 32 MiB");
                }
                output.push_str(">\n");
                stack.push((ns, id, tag));
            }
            0x103 => {
                ensure!(header >= 16, "Invalid end element header");
                let (ns, id, tag) = stack.pop().context("Unexpected XML end element")?;
                ensure!(
                    ns == u32_at(b, header)? && id == u32_at(b, header + 4)?,
                    "Mismatched XML end element"
                );
                output.push_str(&"    ".repeat(stack.len()));
                output.push_str("</");
                output.push_str(&tag);
                output.push_str(">\n");
            }
            0x104 => {
                ensure!(header >= 16 && !stack.is_empty(), "Invalid XML text node");
                output.push_str(&escape(string(&strings, u32_at(b, header)?)?));
                output.push('\n');
            }
            _ => bail!("Unsupported Android XML chunk 0x{ty:04x}"),
        }
        ensure!(output.len() <= LIMIT, "Decoded XML exceeds 32 MiB");
        p += size;
    }
    ensure!(
        roots == 1 && stack.is_empty() && namespaces.is_empty() && pending.is_empty(),
        "Incomplete XML document"
    );
    // Translate sorted byte spans in one pass; metadata must not make decoding quadratic.
    let mut previous = 0;
    let mut chars = 0;
    for range in &mut references {
        chars += output[previous..range.start].chars().count();
        let start = chars;
        chars += output[range.start..range.end].chars().count();
        previous = range.end;
        *range = start..chars;
    }
    Ok((output, references))
}
