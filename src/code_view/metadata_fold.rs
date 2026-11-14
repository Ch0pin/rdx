//! Display-only folding for metadata, imports, and exact original DEX methods.
use std::{borrow::Cow, ops::Range};

pub const SUMMARY: &str = "@Metadata — Kotlin compiler metadata";
const DEX_HEADING: &str = "Original DEX\n";

#[derive(Clone, Debug)]
struct OriginalPayload {
    bytes: Range<usize>,
    chars: Range<usize>,
}

#[derive(Clone, Debug)]
pub enum FoldKind {
    Metadata,
    Imports(usize),
    OriginalDex,
}

#[derive(Clone, Debug)]
pub struct MetadataFold {
    pub bytes: Range<usize>,
    pub chars: Range<usize>,
    pub expanded: bool,
    pub kind: FoldKind,
    original: Option<OriginalPayload>,
}

impl MetadataFold {
    pub fn summary(&self) -> Cow<'static, str> {
        match self.kind {
            FoldKind::Metadata => Cow::Borrowed(SUMMARY),
            FoldKind::Imports(count) => Cow::Owned(format!("imports ({count})")),
            FoldKind::OriginalDex => Cow::Borrowed("Original DEX"),
        }
    }
    pub fn tooltip(&self) -> &'static str {
        match (&self.kind, self.expanded) {
            (FoldKind::Metadata, false) => "Expand Kotlin compiler metadata",
            (FoldKind::Metadata, true) => "Collapse Kotlin compiler metadata",
            (FoldKind::Imports(_), false) => "Expand imports",
            (FoldKind::Imports(_), true) => "Collapse imports",
            (FoldKind::OriginalDex, false) => "Expand exact original DEX method",
            (FoldKind::OriginalDex, true) => "Collapse original DEX method",
        }
    }
    pub fn expanded_header_len(&self, source: &str) -> usize {
        match self.kind {
            FoldKind::Imports(_) => 6,
            FoldKind::OriginalDex => "Original DEX".len(),
            FoldKind::Metadata => {
                if source[self.bytes.start..].starts_with("@kotlin.Metadata") {
                    16
                } else {
                    9
                }
            }
        }
    }

    pub fn detect_all(source: &str, syntax: &str) -> Vec<Self> {
        if syntax != "java" {
            return Vec::new();
        }
        let mut folds = Vec::new();
        let code_lines = code_line_starts(source);
        let imported = source
            .lines()
            .zip(&code_lines)
            .any(|(line, code)| *code && line.trim() == "import kotlin.Metadata;");
        let mut offset = 0;
        for (line, code) in source.split_inclusive('\n').zip(code_lines) {
            if !code {
                offset += line.len();
                continue;
            }
            let leading = line.len() - line.trim_start().len();
            let annotation = &line[leading..];
            let prefix = if annotation.starts_with("@kotlin.Metadata(") {
                "@kotlin.Metadata"
            } else if imported && annotation.starts_with("@Metadata(") {
                "@Metadata"
            } else {
                offset += line.len();
                continue;
            };
            let start = offset + leading;
            let open = start + prefix.len() + 1;
            let Some(end) = balanced_end(source.as_bytes(), open) else {
                offset += line.len();
                continue;
            };
            // Keep indentation and the terminating newline in the projected row.
            if !source[end..]
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                offset += line.len();
                continue;
            }
            let line_end = source[end..].find('\n').map_or(source.len(), |n| end + n);
            let bytes = start..line_end;
            let chars = source[..start].chars().count()..source[..line_end].chars().count();
            folds.push(Self {
                bytes,
                chars,
                expanded: false,
                kind: FoldKind::Metadata,
                original: None,
            });
            offset += line.len();
        }
        folds
    }
    #[cfg(test)]
    pub fn detect(source: &str, syntax: &str) -> Option<Self> {
        Self::detect_all(source, syntax).into_iter().next()
    }

    pub fn collapsed(&self) -> bool {
        !self.expanded
    }
    fn projected_len(&self) -> usize {
        if self.collapsed() {
            self.summary().chars().count()
        } else if let Some(original) = &self.original {
            DEX_HEADING.chars().count() + original.chars.len()
        } else {
            self.chars.len()
        }
    }
    fn projected(&self) -> bool {
        self.collapsed() || self.original.is_some()
    }
    fn projected_text<'a>(&self, source: &'a str) -> Cow<'a, str> {
        if self.collapsed() {
            Cow::Owned(self.summary().into_owned())
        } else if let Some(original) = &self.original {
            Cow::Owned(format!("{DEX_HEADING}{}", &source[original.bytes.clone()]))
        } else {
            Cow::Borrowed(&source[self.bytes.clone()])
        }
    }
    #[cfg(test)]
    pub fn display(&self, source: &str) -> String {
        MetadataFolds {
            folds: vec![self.clone()],
        }
        .display(source)
    }
    #[cfg(test)]
    pub fn summary_chars(&self) -> Range<usize> {
        self.chars.start..self.chars.start + self.summary().chars().count()
    }
    #[cfg(test)]
    pub fn source_to_display(&self, source: usize) -> usize {
        MetadataFolds {
            folds: vec![self.clone()],
        }
        .source_to_display(source)
    }
    #[cfg(test)]
    pub fn display_to_source(&self, display: usize) -> Option<usize> {
        MetadataFolds {
            folds: vec![self.clone()],
        }
        .display_to_source(display)
    }
    pub fn overlaps(&self, range: &Range<usize>) -> bool {
        range.start < self.chars.end && self.chars.start < range.end
    }
}

#[derive(Clone, Debug)]
pub struct MetadataFolds {
    pub folds: Vec<MetadataFold>,
}
impl MetadataFolds {
    pub fn detect(source: &str, syntax: &str) -> Option<Self> {
        let mut folds = MetadataFold::detect_all(source, syntax);
        folds.extend(import_folds(source, syntax));
        let original_dex = original_dex_folds(source, syntax);
        // DEX strings/annotations can resemble Java preamble or metadata. A
        // containing original-method fold owns that range exclusively.
        folds.retain(|fold| {
            !original_dex
                .iter()
                .any(|dex| fold.bytes.start < dex.bytes.end && dex.bytes.start < fold.bytes.end)
        });
        folds.extend(original_dex);
        folds.sort_by_key(|fold| fold.bytes.start);
        (!folds.is_empty()).then_some(Self { folds })
    }
    pub fn collapsed(&self) -> bool {
        self.folds.iter().any(MetadataFold::collapsed)
    }
    pub fn projected(&self) -> bool {
        self.collapsed() || self.folds.iter().any(|fold| fold.original.is_some())
    }
    pub fn display(&self, source: &str) -> String {
        let mut text = source.to_owned();
        for fold in self.folds.iter().rev().filter(|fold| fold.projected()) {
            text.replace_range(fold.bytes.clone(), &fold.projected_text(source));
        }
        text
    }
    pub fn source_to_display(&self, source: usize) -> usize {
        let mut removed = 0isize;
        for fold in self.folds.iter().filter(|fold| fold.projected()) {
            if source <= fold.chars.start {
                break;
            }
            let start = (fold.chars.start as isize - removed) as usize;
            if source < fold.chars.end {
                return if !fold.collapsed()
                    && let Some(original) = &fold.original
                {
                    if source < original.chars.start {
                        start
                    } else {
                        start + DEX_HEADING.chars().count() + source.min(original.chars.end)
                            - original.chars.start
                    }
                } else {
                    start
                };
            }
            removed += fold.chars.len() as isize - fold.projected_len() as isize;
        }
        (source as isize - removed) as usize
    }
    pub fn display_range(&self, fold: &MetadataFold) -> Range<usize> {
        let start = self.source_to_display(fold.chars.start);
        start..start + fold.projected_len()
    }
    pub fn display_to_source(&self, display: usize) -> Option<usize> {
        let mut removed = 0isize;
        for fold in self.folds.iter().filter(|fold| fold.projected()) {
            let start = (fold.chars.start as isize - removed) as usize;
            if display < start {
                break;
            }
            if display < start + fold.projected_len() {
                return if !fold.collapsed()
                    && let Some(original) = &fold.original
                {
                    let relative = display - start;
                    (relative >= DEX_HEADING.chars().count())
                        .then(|| original.chars.start + relative - DEX_HEADING.chars().count())
                } else {
                    None
                };
            }
            removed += fold.chars.len() as isize - fold.projected_len() as isize;
        }
        Some((display as isize + removed) as usize)
    }
    pub fn source_boundary(&self, display: usize, end: bool) -> usize {
        self.display_to_source(display).unwrap_or_else(|| {
            let fold = self
                .folds
                .iter()
                .find(|fold| fold.projected() && self.display_range(fold).contains(&display))
                .unwrap();
            if end {
                fold.original
                    .as_ref()
                    .filter(|_| fold.expanded)
                    .map_or(fold.chars.end, |original| original.chars.start)
            } else {
                fold.chars.start
            }
        })
    }
    /// Clipboard ranges are source ranges; metadata/imports remain exact while
    /// Original DEX protocol headers/footers become a clean heading and payload.
    pub fn copy_source_range(&self, source: &str, range: Range<usize>) -> String {
        let mut edits = Vec::new();
        for fold in &self.folds {
            if let Some(original) = &fold.original {
                edits.push((fold.chars.start..original.chars.start, DEX_HEADING));
                edits.push((original.chars.end..fold.chars.end, ""));
            }
        }
        let mut chars = source.chars().skip(range.start);
        let mut cursor = range.start;
        let mut text = String::new();
        for (removed, replacement) in edits {
            let start = removed.start.max(range.start);
            let end = removed.end.min(range.end);
            if start >= end {
                continue;
            }
            text.extend(chars.by_ref().take(start - cursor));
            chars.by_ref().take(end - start).for_each(drop);
            text.push_str(replacement);
            cursor = end;
        }
        text.extend(chars.take(range.end.saturating_sub(cursor)));
        text
    }
    pub fn copy_display_range(&self, source: &str, range: Range<usize>) -> String {
        let mut text = String::new();
        let mut source_start = 0;
        let mut display_start = 0;
        let append = |text: &mut String, original: Range<usize>, displayed: usize| {
            let start = range.start.max(displayed);
            let end = range.end.min(displayed + original.len());
            if start < end {
                text.extend(
                    source
                        .chars()
                        .skip(original.start + start - displayed)
                        .take(end - start),
                );
            }
        };
        for fold in self.folds.iter().filter(|fold| fold.projected()) {
            append(&mut text, source_start..fold.chars.start, display_start);
            display_start += fold.chars.start - source_start;
            let finish = display_start + fold.projected_len();
            if fold.collapsed() {
                if range.start < finish && display_start < range.end {
                    text.push_str(&self.copy_source_range(source, fold.chars.clone()));
                }
            } else if let Some(original) = &fold.original {
                let end = range.end.min(display_start + DEX_HEADING.chars().count());
                let start = range.start.max(display_start);
                if start < end {
                    text.extend(
                        DEX_HEADING
                            .chars()
                            .skip(start - display_start)
                            .take(end - start),
                    );
                }
                append(
                    &mut text,
                    original.chars.clone(),
                    display_start + DEX_HEADING.chars().count(),
                );
            }
            source_start = fold.chars.end;
            display_start = finish;
        }
        append(
            &mut text,
            source_start..source.chars().count(),
            display_start,
        );
        text
    }
}

fn original_dex_folds(source: &str, syntax: &str) -> Vec<MetadataFold> {
    use rdx::alternate_source::{ORIGINAL_DEX_BEGIN, ORIGINAL_DEX_END};
    if syntax != "java" {
        return Vec::new();
    }
    let mut folds = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find(ORIGINAL_DEX_BEGIN) {
        let start = cursor + relative;
        cursor = start + ORIGINAL_DEX_BEGIN.len();
        if start > 0 && source.as_bytes()[start - 1] != b'\n' {
            continue;
        }
        let Some(header_end) = source[cursor..].find('\n').map(|offset| cursor + offset) else {
            break;
        };
        let Ok(length) = source[cursor..header_end].parse::<usize>() else {
            continue;
        };
        let body_start = header_end + 1;
        let Some(body_end) = body_start
            .checked_add(length)
            .filter(|end| *end <= source.len() && source.is_char_boundary(*end))
        else {
            continue;
        };
        let original = &source[body_start..body_end];
        if rdx::alternate_source::original_dex_method_offset(original).is_none()
            || !source[body_end..].starts_with(ORIGINAL_DEX_END)
        {
            continue;
        }
        let end = body_end + ORIGINAL_DEX_END.len();
        if source
            .as_bytes()
            .get(end)
            .is_some_and(|byte| *byte != b'\n')
        {
            continue;
        }
        folds.push(MetadataFold {
            bytes: start..end,
            chars: source[..start].chars().count()..source[..end].chars().count(),
            expanded: false,
            kind: FoldKind::OriginalDex,
            original: Some(OriginalPayload {
                bytes: body_start..body_end,
                chars: source[..body_start].chars().count()..source[..body_end].chars().count(),
            }),
        });
        cursor = end;
    }
    folds
}

// Restrict import folding to complete declarations in the Java preamble.
// Blank lines may separate normal/static imports; comments remain visible.
fn import_folds(source: &str, syntax: &str) -> Vec<MetadataFold> {
    if syntax != "java" {
        return Vec::new();
    }
    let mut folds = Vec::new();
    let mut block: Option<(usize, usize, usize)> = None;
    let flush = |block: &mut Option<(usize, usize, usize)>, folds: &mut Vec<MetadataFold>| {
        if let Some((start, end, count)) = block.take()
            && count > 1
        {
            folds.push(MetadataFold {
                bytes: start..end,
                chars: source[..start].chars().count()..source[..end].chars().count(),
                expanded: false,
                kind: FoldKind::Imports(count),
                original: None,
            });
        }
    };
    let mut offset = 0;
    for (line, code) in source.split_inclusive('\n').zip(code_line_starts(source)) {
        let text = line.trim();
        if text.is_empty() {
            offset += line.len();
            continue;
        }
        if !code {
            flush(&mut block, &mut folds);
            offset += line.len();
            continue;
        }
        let import = text
            .strip_prefix("import ")
            .and_then(|s| s.strip_suffix(';'))
            .map(|s| s.strip_prefix("static ").unwrap_or(s))
            .filter(|s| {
                let parts: Vec<_> = s.split('.').collect();
                parts.len() >= 2
                    && parts.iter().enumerate().all(|(i, part)| {
                        if *part == "*" {
                            return i + 1 == parts.len();
                        }
                        let mut chars = part.chars();
                        chars
                            .next()
                            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                            && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    })
            });
        if import.is_some() {
            let start = offset + line.len() - line.trim_start().len();
            let end = offset + line.trim_end_matches('\n').len();
            let entry = block.get_or_insert((start, end, 0));
            entry.1 = end;
            entry.2 += 1;
        } else if text.starts_with("package ") && block.is_none() {
            // The emitter places the package declaration before imports.
        } else {
            break;
        }
        offset += line.len();
    }
    flush(&mut block, &mut folds);
    folds
}

// Whether the first non-whitespace token on each line is Java code. A comment
// or text block can contain convincing annotation/import lines.
fn code_line_starts(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    let mut block = false;
    let mut text_block = false;
    while i < bytes.len() {
        let end = bytes[i..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |n| i + n);
        let mut first_code = false;
        let mut first_token = block || text_block;
        while i < end {
            if block {
                if bytes.get(i..i + 2) == Some(b"*/") {
                    block = false;
                    i += 2;
                } else {
                    i += 1;
                }
            } else if text_block {
                if bytes.get(i..i + 3) == Some(b"\"\"\"") {
                    text_block = false;
                    i += 3;
                } else {
                    i += 1;
                }
            } else if bytes.get(i..i + 2) == Some(b"/*") {
                first_token = true;
                block = true;
                i += 2;
            } else if bytes.get(i..i + 2) == Some(b"//") {
                break;
            } else if bytes.get(i..i + 3) == Some(b"\"\"\"") {
                if !first_token {
                    first_code = true;
                    first_token = true;
                }
                text_block = true;
                i += 3;
            } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                if !first_token {
                    first_code = true;
                    first_token = true;
                }
                let quote = bytes[i];
                i += 1;
                while i < end {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(end);
                    } else if bytes[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            } else if bytes[i].is_ascii_whitespace() {
                i += 1;
            } else {
                if !first_token {
                    first_code = true;
                    first_token = true;
                }
                i += 1;
            }
        }
        result.push(first_code);
        i = end + usize::from(end < bytes.len());
    }
    result
}

fn balanced_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = open;
    let mut quote = false;
    while i < bytes.len() {
        if quote {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => {
                    quote = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }
        match bytes[i] {
            b'"' => {
                quote = true;
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let rest = bytes[i + 2..].windows(2).position(|pair| pair == b"*/")?;
                i += rest + 4;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |n| i + n);
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_dex_folds_below_visible_warning_and_above_reconstruction() {
        use rdx::alternate_source::{ORIGINAL_DEX_BEGIN, ORIGINAL_DEX_END, UNVERIFIED_WARNING};
        let original = ".method sample.Café.a()V // access=0x1\n    // @kotlin.Metadata(\"fake\")\n    return-void\n.end method\n";
        let source = format!(
            "import kotlin.Metadata;\nimport java.util.List;\n// {UNVERIFIED_WARNING}\n{ORIGINAL_DEX_BEGIN}{}\n{original}{ORIGINAL_DEX_END}\nvoid a() {{ broken(); }}\n",
            original.len()
        );
        let mut group = MetadataFolds::detect(&source, "java").unwrap();
        let dex = group
            .folds
            .iter()
            .position(|fold| matches!(fold.kind, FoldKind::OriginalDex))
            .unwrap();
        assert!(group.folds[dex].collapsed());
        let display = group.display(&source);
        assert!(display.contains(&format!("// {UNVERIFIED_WARNING}\nOriginal DEX\nvoid a()")));
        assert!(!display.contains("return-void"));
        let method = source[..source.find("void a()").unwrap()].chars().count();
        let displayed = display[..display.find("void a()").unwrap()].chars().count();
        assert_eq!(group.source_to_display(method), displayed);
        assert_eq!(group.display_to_source(displayed), Some(method));
        group.folds[dex].expanded = true;
        assert!(group.display(&source).contains(original));
    }

    #[test]
    fn multiple_original_methods_use_exact_lengths_and_nonoverlapping_folds() {
        use rdx::alternate_source::{ORIGINAL_DEX_BEGIN, ORIGINAL_DEX_END};
        let raw = ".method sample.A.a()V\n@kotlin.Metadata(x=1)\n.end method\n";
        let block = format!(
            "{ORIGINAL_DEX_BEGIN}{}\n{raw}{ORIGINAL_DEX_END}\n",
            raw.len()
        );
        let source = format!("{block}void a() {{}}\n{block}void b() {{}}\n");
        let group = MetadataFolds::detect(&source, "java").unwrap();
        assert_eq!(group.folds.len(), 2);
        assert!(
            group
                .folds
                .iter()
                .all(|fold| matches!(fold.kind, FoldKind::OriginalDex))
        );
        assert!(group.folds[0].bytes.end <= group.folds[1].bytes.start);
        assert!(group.display(&source).contains("void b()"));
        let malformed =
            format!("{ORIGINAL_DEX_BEGIN}999999999999999999\n{raw}{ORIGINAL_DEX_END}\n");
        assert!(original_dex_folds(&malformed, "java").is_empty());
    }
    #[test]
    fn imported_metadata_folds_with_unicode_and_exact_offsets() {
        let source = "import kotlin.Metadata;\n// café\n@Metadata(d1 = {\"a)\\\"b\"},\n d2 = {\"c\"})\nclass Test {}\n";
        let fold = MetadataFold::detect(source, "java").unwrap();
        let display = fold.display(source);
        assert!(display.contains(SUMMARY));
        assert!(!display.contains("d1 ="));
        let class_source = source[..source.find("class Test").unwrap()].chars().count();
        let class_display = display[..display.find("class Test").unwrap()]
            .chars()
            .count();
        assert_eq!(fold.source_to_display(class_source), class_display);
        assert_eq!(fold.display_to_source(class_display), Some(class_source));
        assert_eq!(fold.display_to_source(fold.summary_chars().start + 2), None);
        assert!(fold.overlaps(&(fold.chars.start - 1..fold.chars.start + 2)));
    }
    #[test]
    fn unrelated_metadata_is_left_alone() {
        assert!(MetadataFold::detect("@Metadata(value = 1)\nclass A {}", "java").is_none());
        assert!(MetadataFold::detect("@kotlin.Metadata(\"x\")\nclass A {}", "xml").is_none());
        assert!(MetadataFold::detect("@interface Metadata {}\n@Metadata(x = 1)", "java").is_none());
        assert!(
            MetadataFold::detect("/*\n@kotlin.Metadata(x = 1)\n*/\nclass A {}", "java").is_none()
        );
        assert!(
            MetadataFold::detect(
                "/* import kotlin.Metadata; */\n@Metadata(x = 1)\nclass A {}",
                "java"
            )
            .is_none()
        );
        assert!(
            MetadataFold::detect(
                "String example = \"\"\"\n@kotlin.Metadata(d1={\"x\"})\n\"\"\";",
                "java"
            )
            .is_none()
        );
        assert!(
            MetadataFold::detect("class A { /*\n@kotlin.Metadata(d1={\"x\"})\n*/ }", "java")
                .is_none()
        );
    }
}

#[cfg(test)]
mod multiple_tests {
    use super::*;
    #[test]
    fn nested_folds_preserve_unicode_mappings_and_expand_independently() {
        let source = "import kotlin.Metadata;\n@Metadata(d1={\"α😀\"})\nclass Outer {\n @Metadata(d1={\"β\",\n \"γ\"})\n class Inner {}\n}\n";
        for mask in 0u32..4 {
            let mut group = MetadataFolds::detect(source, "java").unwrap();
            assert_eq!(group.folds.len(), 2);
            for (i, fold) in group.folds.iter_mut().enumerate() {
                fold.expanded = mask & (1 << i) != 0;
            }
            let display = group.display(source);
            assert_eq!(
                display.matches(SUMMARY).count(),
                2 - mask.count_ones() as usize
            );
            for pos in 0..=source.chars().count() {
                if group
                    .folds
                    .iter()
                    .any(|f| f.collapsed() && f.chars.contains(&pos))
                {
                    continue;
                }
                assert_eq!(
                    group.display_to_source(group.source_to_display(pos)),
                    Some(pos),
                    "mask={mask},pos={pos}"
                );
            }
            for fold in group.folds.iter().filter(|f| f.collapsed()) {
                let range = group.display_range(fold);
                assert_eq!(group.source_boundary(range.start, false), fold.chars.start);
                assert_eq!(group.source_boundary(range.start, true), fold.chars.end);
            }
        }
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    #[test]
    fn imports_and_metadata_expand_independently_with_exact_unicode_mappings() {
        let source = "// café😀\npackage example;\nimport kotlin.Metadata;\nimport java.util.List;\n\nimport static java.util.Collections.*;\n@Metadata(d1={\"β\"})\nclass Example {}\n";
        for mask in 0u32..4 {
            let mut group = MetadataFolds::detect(source, "java").unwrap();
            assert_eq!(group.folds.len(), 2);
            assert!(matches!(group.folds[0].kind, FoldKind::Imports(3)));
            for (i, fold) in group.folds.iter_mut().enumerate() {
                fold.expanded = mask & (1 << i) != 0;
            }
            let display = group.display(source);
            assert_eq!(display.contains("imports (3)"), mask & 1 == 0);
            assert_eq!(display.contains(SUMMARY), mask & 2 == 0);
            for pos in 0..=source.chars().count() {
                if group
                    .folds
                    .iter()
                    .any(|f| f.collapsed() && f.chars.contains(&pos))
                {
                    continue;
                }
                assert_eq!(
                    group.display_to_source(group.source_to_display(pos)),
                    Some(pos)
                );
            }
            for fold in group.folds.iter().filter(|f| f.collapsed()) {
                let range = group.display_range(fold);
                assert_eq!(group.source_boundary(range.start, false), fold.chars.start);
                assert_eq!(group.source_boundary(range.start, true), fold.chars.end);
            }
        }
    }
    #[test]
    fn comments_stay_visible_and_import_lookalikes_do_not_fold() {
        let source = "/*\nimport fake.A;\nimport fake.B;\n*/\nimport real.A;\nimport real.B;\n// useful comment\nimport real.C;\nimport real.D;\nclass Example {}\n";
        let group = MetadataFolds::detect(source, "java").unwrap();
        assert_eq!(group.folds.len(), 2);
        assert!(group.display(source).contains("// useful comment"));
        assert!(group.display(source).contains("import fake.A;"));
        for source in [
            "import one.A;\nclass Example {}",
            "class Example {\nimport fake.A;\nimport fake.B;\n}",
            "String example = \"\"\"\nimport fake.A;\nimport fake.B;\n\"\"\";",
            ".method example.A()V\nimport fake.A;\nimport fake.B;",
            "import broken..A;\nimport real.B;",
        ] {
            assert!(MetadataFolds::detect(source, "java").is_none(), "{source}");
        }
        assert!(MetadataFolds::detect("import real.A;\nimport real.B;", "xml").is_none());
    }
}
