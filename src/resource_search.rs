//! Numeric aliases for proven resource links, using displayed-source offsets.
use crate::engine::CodeLink;
use std::ops::Range;

pub fn query_id(query: &str) -> Option<u32> {
    if let Some(hex) = query
        .strip_prefix("0x")
        .or_else(|| query.strip_prefix("0X"))
    {
        (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
    } else {
        (!query.is_empty() && query.bytes().all(|b| b.is_ascii_digit()))
            .then(|| query.parse().ok())
            .flatten()
    }
}

pub fn link_id(link: &CodeLink) -> Option<u32> {
    let id = link.label.strip_prefix("resource://")?.split('/').next()?;
    (id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(id, 16).ok())
        .flatten()
}

pub fn aliases<'a>(links: &'a [CodeLink], query: &str) -> impl Iterator<Item = Range<usize>> + 'a {
    let id = query_id(query);
    links
        .iter()
        .filter(move |link| id.is_some() && link_id(link) == id)
        .map(|link| link.start..link.end)
}

/// Convert sorted character ranges once, avoiding repeated full-source scans.
pub fn byte_aliases(source: &str, links: &[CodeLink], query: &str) -> Vec<Range<usize>> {
    let mut ranges: Vec<_> = aliases(links, query).collect();
    ranges.sort_by_key(|r| (r.start, r.end));
    ranges.dedup();
    let mut positions = source
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([source.len()])
        .enumerate();
    let mut at = positions.next();
    let mut result = Vec::new();
    for range in ranges {
        while at.is_some_and(|(index, _)| index < range.start) {
            at = positions.next();
        }
        let Some((index, start)) = at else { break };
        if index != range.start || range.start >= range.end {
            continue;
        }
        while at.is_some_and(|(index, _)| index < range.end) {
            at = positions.next();
        }
        if let Some((index, end)) = at
            && index == range.end
        {
            result.push(start..end);
        }
    }
    result
}

/// Merge ordinary literal matches with numeric aliases in source order.
pub fn literal_ranges<'a>(
    source: &'a str,
    links: &[CodeLink],
    query: &'a str,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let mut text = source
        .match_indices(query)
        .map(|(start, value)| start..start + value.len())
        .peekable();
    let mut aliases = byte_aliases(source, links, query).into_iter().peekable();
    std::iter::from_fn(move || match (text.peek(), aliases.peek()) {
        (Some(a), Some(b)) if a == b => {
            aliases.next();
            text.next()
        }
        (Some(a), Some(b)) if (a.start, a.end) <= (b.start, b.end) => text.next(),
        (Some(_), Some(_)) => aliases.next(),
        (Some(_), None) => text.next(),
        (None, Some(_)) => aliases.next(),
        (None, None) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_aliases_preserve_unicode_display_offsets() {
        let source = "é R.anim.fade";
        let links = vec![CodeLink {
            start: 2,
            end: 13,
            label: "resource://7f010066/anim/fade | preview".into(),
        }];
        for query in ["2130772070", "0x7f010066", "0X7F010066"] {
            assert_eq!(aliases(&links, query).collect::<Vec<_>>(), vec![2..13]);
            assert_eq!(byte_aliases(source, &links, query), vec![3..14]);
        }
        for query in ["213077207", "-2130772070", "0x", "resource", "fade"] {
            assert_eq!(aliases(&links, query).count(), 0);
        }
    }

    #[test]
    fn numeric_aliases_and_literals_page_in_display_order() {
        let source = "é 2130772070 R.anim.fade 2130772070";
        let links = vec![CodeLink {
            start: 13,
            end: 24,
            label: "resource://7f010066/anim/fade".into(),
        }];
        let matches: Vec<_> = literal_ranges(source, &links, "2130772070").collect();
        assert_eq!(matches, vec![3..13, 14..25, 26..36]);
        assert_eq!(
            literal_ranges(source, &links, "2130772070")
                .filter(|r| r.start >= matches[1].end)
                .collect::<Vec<_>>(),
            vec![26..36]
        );
    }
}
