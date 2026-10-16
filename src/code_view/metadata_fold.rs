//! Display-only folding for Kotlin metadata and Java import blocks.
use std::{borrow::Cow, ops::Range};

pub const SUMMARY: &str = "@Metadata — Kotlin compiler metadata";

#[derive(Clone, Debug)]
pub enum FoldKind {
    Metadata,
    Imports(usize),
}

#[derive(Clone, Debug)]
pub struct MetadataFold {
    pub bytes: Range<usize>,
    pub chars: Range<usize>,
    pub expanded: bool,
    pub kind: FoldKind,
}

impl MetadataFold {
    pub fn summary(&self) -> Cow<'static, str> {
        match self.kind {
            FoldKind::Metadata => Cow::Borrowed(SUMMARY),
            FoldKind::Imports(count) => Cow::Owned(format!("imports ({count})")),
        }
    }
    pub fn tooltip(&self) -> &'static str {
        match (&self.kind, self.expanded) {
            (FoldKind::Metadata, false) => "Expand Kotlin compiler metadata",
            (FoldKind::Metadata, true) => "Collapse Kotlin compiler metadata",
            (FoldKind::Imports(_), false) => "Expand imports",
            (FoldKind::Imports(_), true) => "Collapse imports",
        }
    }
    pub fn expanded_header_len(&self, source: &str) -> usize {
        match self.kind {
            FoldKind::Imports(_) => 6,
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
    #[cfg(test)]
    pub fn display(&self, source: &str) -> String {
        if self.expanded {
            return source.to_owned();
        }
        let mut text =
            String::with_capacity(source.len() - self.bytes.len() + self.summary().len());
        text.push_str(&source[..self.bytes.start]);
        text.push_str(&self.summary());
        text.push_str(&source[self.bytes.end..]);
        text
    }
    #[cfg(test)]
    pub fn summary_chars(&self) -> Range<usize> {
        self.chars.start..self.chars.start + self.summary().chars().count()
    }
    #[cfg(test)]
    pub fn source_to_display(&self, source: usize) -> usize {
        if self.expanded || source <= self.chars.start {
            source
        } else if source < self.chars.end {
            self.chars.start
        } else {
            source - self.chars.len() + self.summary().chars().count()
        }
    }
    #[cfg(test)]
    pub fn display_to_source(&self, display: usize) -> Option<usize> {
        if self.expanded || display < self.chars.start {
            Some(display)
        } else if display < self.summary_chars().end {
            None
        } else {
            Some(display - self.summary().chars().count() + self.chars.len())
        }
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
        folds.sort_by_key(|fold| fold.bytes.start);
        (!folds.is_empty()).then_some(Self { folds })
    }
    pub fn collapsed(&self) -> bool {
        self.folds.iter().any(MetadataFold::collapsed)
    }
    pub fn display(&self, source: &str) -> String {
        let mut text = source.to_owned();
        for fold in self.folds.iter().rev().filter(|f| f.collapsed()) {
            text.replace_range(fold.bytes.clone(), &fold.summary());
        }
        text
    }
    pub fn source_to_display(&self, source: usize) -> usize {
        let mut removed = 0;
        for fold in self.folds.iter().filter(|f| f.collapsed()) {
            if source <= fold.chars.start {
                break;
            }
            if source < fold.chars.end {
                return (fold.chars.start as isize - removed) as usize;
            }
            removed += fold.chars.len() as isize - fold.summary().chars().count() as isize;
        }
        (source as isize - removed) as usize
    }
    pub fn display_range(&self, fold: &MetadataFold) -> Range<usize> {
        let start = self.source_to_display(fold.chars.start);
        start
            ..start
                + if fold.collapsed() {
                    fold.summary().chars().count()
                } else {
                    fold.chars.len()
                }
    }
    pub fn display_to_source(&self, display: usize) -> Option<usize> {
        let mut source = display as isize;
        for fold in self.folds.iter().filter(|f| f.collapsed()) {
            let range = self.display_range(fold);
            if display < range.start {
                break;
            }
            if display < range.end {
                return None;
            }
            source += fold.chars.len() as isize - fold.summary().chars().count() as isize;
        }
        Some(source as usize)
    }
    pub fn source_boundary(&self, display: usize, end: bool) -> usize {
        self.display_to_source(display).unwrap_or_else(|| {
            let fold = self
                .folds
                .iter()
                .find(|f| f.collapsed() && self.display_range(f).contains(&display))
                .unwrap();
            if end {
                fold.chars.end
            } else {
                fold.chars.start
            }
        })
    }
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
