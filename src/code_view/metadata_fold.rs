//! A display-only projection for the compiler's Kotlin metadata annotation.
use std::ops::Range;

pub const SUMMARY: &str = "@Metadata — Kotlin compiler metadata";

#[derive(Clone, Debug)]
pub struct MetadataFold {
    pub bytes: Range<usize>,
    pub chars: Range<usize>,
    pub expanded: bool,
}

impl MetadataFold {
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
        let mut text = String::with_capacity(source.len() - self.bytes.len() + SUMMARY.len());
        text.push_str(&source[..self.bytes.start]);
        text.push_str(SUMMARY);
        text.push_str(&source[self.bytes.end..]);
        text
    }
    #[cfg(test)]
    pub fn summary_chars(&self) -> Range<usize> {
        self.chars.start..self.chars.start + SUMMARY.chars().count()
    }
    #[cfg(test)]
    pub fn source_to_display(&self, source: usize) -> usize {
        if self.expanded || source <= self.chars.start {
            source
        } else if source < self.chars.end {
            self.chars.start
        } else {
            source - self.chars.len() + SUMMARY.chars().count()
        }
    }
    #[cfg(test)]
    pub fn display_to_source(&self, display: usize) -> Option<usize> {
        if self.expanded || display < self.chars.start {
            Some(display)
        } else if display < self.summary_chars().end {
            None
        } else {
            Some(display - SUMMARY.chars().count() + self.chars.len())
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
        let folds = MetadataFold::detect_all(source, syntax);
        (!folds.is_empty()).then_some(Self { folds })
    }
    pub fn collapsed(&self) -> bool {
        self.folds.iter().any(MetadataFold::collapsed)
    }
    pub fn display(&self, source: &str) -> String {
        let mut text = source.to_owned();
        for fold in self.folds.iter().rev().filter(|f| f.collapsed()) {
            text.replace_range(fold.bytes.clone(), SUMMARY);
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
            removed += fold.chars.len() as isize - SUMMARY.chars().count() as isize;
        }
        (source as isize - removed) as usize
    }
    pub fn display_range(&self, fold: &MetadataFold) -> Range<usize> {
        let start = self.source_to_display(fold.chars.start);
        start
            ..start
                + if fold.collapsed() {
                    SUMMARY.chars().count()
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
            source += fold.chars.len() as isize - SUMMARY.chars().count() as isize;
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
