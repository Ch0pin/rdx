//! Exact Java literals from the DEX reader's reversible display representation.
use anyhow::{Context, Result, bail, ensure};

pub(super) fn literal(display: &str) -> Result<String> {
    let mut out = String::from("\"");
    let mut input = display.chars();
    while let Some(ch) = input.next() {
        match ch {
            '\\' => match input
                .next()
                .context("truncated DEX string display escape")?
            {
                '\\' => out.push_str("\\\\"),
                'u' => {
                    ensure!(
                        input.next() == Some('{'),
                        "invalid DEX string display escape"
                    );
                    let mut value = 0u16;
                    for _ in 0..4 {
                        let digit = input
                            .next()
                            .and_then(|c| c.to_digit(16))
                            .context("invalid DEX surrogate display escape")?;
                        value = (value << 4) | digit as u16;
                    }
                    ensure!(
                        input.next() == Some('}') && (0xd800..=0xdfff).contains(&value),
                        "invalid DEX surrogate display escape"
                    );
                    out.push_str(&format!("\\u{value:04x}"));
                }
                _ => bail!("invalid DEX string display escape"),
            },
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if c.is_control() && (c as u32) <= 255 => {
                out.push_str(&format!("\\{:03o}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent decoder for the Java literal subset emitted here. Its UTF-16
    // result can retain lone surrogate units without requiring a Java runtime.
    fn units(java: &str) -> Vec<u16> {
        let mut result = Vec::new();
        let mut chars = java
            .strip_prefix('"')
            .unwrap()
            .strip_suffix('"')
            .unwrap()
            .chars()
            .peekable();
        while let Some(c) = chars.next() {
            if c != '\\' {
                result.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
                continue;
            }
            let c = chars.next().unwrap();
            let unit = match c {
                '\\' => 92,
                '"' => 34,
                'b' => 8,
                'f' => 12,
                'n' => 10,
                'r' => 13,
                't' => 9,
                'u' => (0..4).fold(0u16, |n, _| {
                    (n << 4) | chars.next().unwrap().to_digit(16).unwrap() as u16
                }),
                '0'..='3' => {
                    let mut n = c.to_digit(8).unwrap() as u16;
                    for _ in 0..2 {
                        if chars.peek().is_some_and(|c| matches!(c, '0'..='7')) {
                            n = n * 8 + chars.next().unwrap().to_digit(8).unwrap() as u16;
                        }
                    }
                    n
                }
                _ => panic!("unexpected Java escape"),
            };
            result.push(unit);
        }
        result
    }

    #[test]
    fn backslashes_surrogates_unicode_and_controls_roundtrip_exactly() {
        let cases = [
            (
                r"C:\\temp\\note",
                "C:\\temp\\note".encode_utf16().collect::<Vec<_>>(),
            ),
            (r"\u{d800}x\u{dfff}", vec![0xd800, 120, 0xdfff]),
            (r"\\u{d800}", "\\u{d800}".encode_utf16().collect()),
            ("Ελληνικά 🦀", "Ελληνικά 🦀".encode_utf16().collect()),
            (
                "\"\n\r\t\0\u{0008}\u{000c}\u{001a}\u{007f}",
                vec![34, 10, 13, 9, 0, 8, 12, 26, 127],
            ),
        ];
        for (display, expected) in cases {
            assert_eq!(units(&literal(display).unwrap()), expected, "{display:?}");
        }
    }
    #[test]
    fn literal_unicode_escape_text_is_not_interpreted_as_a_line_break() {
        assert_eq!(literal(r"\\u000a").unwrap(), r#""\\u000a""#);
        assert_eq!(
            units(&literal(r"\\u000a").unwrap()),
            "\\u000a".encode_utf16().collect::<Vec<_>>()
        );
    }
    #[test]
    fn malformed_display_escapes_are_rejected() {
        for display in ["\\", r"\n", r"\u{0041}", r"\u{d80}", r"\u{d800", r"\uD800"] {
            assert!(literal(display).is_err(), "{display}");
        }
    }
}
