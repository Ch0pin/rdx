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
    pub fn detect(source: &str, syntax: &str) -> Option<Self> {
        if syntax != "java" {
            return None;
        }
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
            if !source[end..].split('\n').next()?.trim().is_empty() {
                offset += line.len();
                continue;
            }
            let line_end = source[end..].find('\n').map_or(source.len(), |n| end + n);
            let bytes = start..line_end;
            let chars = source[..start].chars().count()..source[..line_end].chars().count();
            return Some(Self {
                bytes,
                chars,
                expanded: false,
            });
        }
        None
    }

    pub fn collapsed(&self) -> bool {
        !self.expanded
    }
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
    pub fn summary_chars(&self) -> Range<usize> {
        self.chars.start..self.chars.start + SUMMARY.chars().count()
    }
    pub fn source_to_display(&self, source: usize) -> usize {
        if self.expanded || source <= self.chars.start {
            source
        } else if source < self.chars.end {
            self.chars.start
        } else {
            source - self.chars.len() + SUMMARY.chars().count()
        }
    }
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
