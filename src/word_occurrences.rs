//! Exact word matching using Unicode scalar offsets, as used by the viewer.
use std::{ops::Range, sync::OnceLock};

fn words() -> &'static regex::Regex {
    static WORDS: OnceLock<regex::Regex> = OnceLock::new();
    WORDS.get_or_init(|| regex::Regex::new(r"[\p{L}\p{M}\p{N}_$]+").expect("word pattern"))
}

/// Return the complete word containing a glyph's Unicode scalar index.
pub fn word_at(source: &str, index: usize) -> Option<Range<usize>> {
    let byte = source.char_indices().nth(index)?.0;
    let mut preceding_byte = 0;
    let mut preceding_scalar = 0;
    for token in words().find_iter(source) {
        if token.start() > byte {
            return None;
        }
        if token.end() > byte {
            let start = preceding_scalar + source[preceding_byte..token.start()].chars().count();
            return Some(start..start + token.as_str().chars().count());
        }
        preceding_scalar += source[preceding_byte..token.end()].chars().count();
        preceding_byte = token.end();
    }
    None
}

pub fn selected_word(source: &str, selection: Range<usize>) -> Option<String> {
    if selection.start >= selection.end {
        return None;
    }
    let mut chars = source.char_indices();
    let start = chars.nth(selection.start)?.0;
    let end = match chars.nth(selection.end - selection.start - 1) {
        Some((i, _)) => i,
        None if selection.end == source.chars().count() => source.len(),
        None => return None,
    };
    let token = words().find_at(source, start)?;
    if token.range() != (start..end) {
        return None;
    }
    // find_at can start in the middle of a token, so check its preceding scalar.
    if let Some(previous) = source[..start].chars().next_back() {
        let mut bytes = [0; 4];
        if words().is_match(previous.encode_utf8(&mut bytes)) {
            return None;
        }
    }
    Some(source[start..end].to_owned())
}

pub fn occurrences(source: &str, word: &str) -> Vec<Range<usize>> {
    let mut matches = Vec::new();
    let mut byte = 0;
    let mut scalar = 0;
    for token in words().find_iter(source) {
        if token.as_str() != word {
            continue;
        }
        scalar += source[byte..token.start()].chars().count();
        let end = scalar + token.as_str().chars().count();
        matches.push(scalar..end);
        scalar = end;
        byte = token.end();
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clicked_glyph_expands_unicode_word_and_rejects_separators() {
        let source = "🎯 café $id e\u{301};";
        assert_eq!(word_at(source, 2), Some(2..6));
        assert_eq!(word_at(source, 5), Some(2..6));
        assert_eq!(word_at(source, 7), Some(7..10));
        assert_eq!(word_at(source, 12), Some(11..13));
        for index in [0, 1, 6, 10, 13, 14, usize::MAX] {
            assert_eq!(word_at(source, index), None);
        }
    }
    #[test]
    fn exact_unicode_case_and_identifier_boundaries() {
        let source = "🦀 café café2 CAFÉ\r\ncafé $id $id_more $id e\u{301} e\u{301}";
        assert_eq!(selected_word(source, 2..6).as_deref(), Some("café"));
        assert_eq!(occurrences(source, "café"), vec![2..6, 19..23]);
        assert!(selected_word(source, 3..6).is_none());
        assert!(selected_word(source, 2..5).is_none());
        assert!(selected_word(source, 2..12).is_none());
        assert!(selected_word(source, 0..0).is_none());
        for word in ["$id", "e\u{301}"] {
            let found = occurrences(source, word);
            assert_eq!(found.len(), 2);
            for range in found {
                assert_eq!(selected_word(source, range).as_deref(), Some(word));
            }
        }
        assert!(occurrences("value valueMore Value", "missing").is_empty());
        assert_eq!(occurrences("value valueMore Value", "value"), vec![0..5]);
    }
}
