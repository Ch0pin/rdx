//! Conservative Java presentation proof: exact linked Android API arguments.
//! A matching ARSC integer alone never grants resource identity.
use crate::engine::DecompiledCode;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

struct Token {
    text: String,
    span: Range<usize>,
}

pub(super) fn dex_directive(line: &str) -> bool {
    let line = line.trim_start();
    [
        ".class ",
        ".super ",
        ".implements ",
        ".field ",
        ".registers ",
        ".throws ",
        ".catch ",
        ".catchall ",
        ".end method",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

pub(super) fn arguments(label: &str) -> Option<&'static [(usize, &'static [&'static str])]> {
    Some(match label {
        "android.app.Activity.overridePendingTransition(II)V" => &[(0, &["anim"]), (1, &["anim"])],
        "android.app.Activity.overrideActivityTransition(III)V" => {
            &[(1, &["anim"]), (2, &["anim"])]
        }
        "android.widget.TextView.setText(I)V"
        | "android.widget.TextView.setHint(I)V"
        | "android.content.Context.getString(I)Ljava/lang/String;"
        | "android.content.Context.getString(I[Ljava/lang/Object;)Ljava/lang/String;"
        | "android.content.Context.getText(I)Ljava/lang/CharSequence;"
        | "android.content.res.Resources.getString(I)Ljava/lang/String;"
        | "android.content.res.Resources.getString(I[Ljava/lang/Object;)Ljava/lang/String;"
        | "android.content.res.Resources.getText(I)Ljava/lang/CharSequence;" => &[(0, &["string"])],
        "android.app.Activity.setContentView(I)V"
        | "android.view.LayoutInflater.inflate(ILandroid/view/ViewGroup;)Landroid/view/View;"
        | "android.view.LayoutInflater.inflate(ILandroid/view/ViewGroup;Z)Landroid/view/View;" => {
            &[(0, &["layout"])]
        }
        "android.app.Activity.findViewById(I)Landroid/view/View;"
        | "android.view.View.findViewById(I)Landroid/view/View;"
        | "android.view.View.setId(I)V" => &[(0, &["id"])],
        "android.widget.ImageView.setImageResource(I)V"
        | "android.view.View.setBackgroundResource(I)V"
        | "android.content.res.Resources.getDrawable(I)Landroid/graphics/drawable/Drawable;"
        | "android.content.res.Resources.getDrawable(ILandroid/content/res/Resources$Theme;)Landroid/graphics/drawable/Drawable;" => {
            &[(0, &["drawable", "mipmap", "color"])]
        }
        "android.content.res.Resources.getColor(I)I"
        | "android.content.res.Resources.getColor(ILandroid/content/res/Resources$Theme;)I" => {
            &[(0, &["color"])]
        }
        "android.content.res.Resources.getDimension(I)F"
        | "android.content.res.Resources.getDimensionPixelSize(I)I"
        | "android.content.res.Resources.getDimensionPixelOffset(I)I" => &[(0, &["dimen"])],
        "android.content.res.Resources.getInteger(I)I" => &[(0, &["integer"])],
        "android.content.res.Resources.getBoolean(I)Z" => &[(0, &["bool"])],
        "android.content.res.Resources.getXml(I)Landroid/content/res/XmlResourceParser;" => {
            &[(0, &["xml"])]
        }
        "android.content.res.Resources.getQuantityString(II)Ljava/lang/String;"
        | "android.content.res.Resources.getQuantityString(II[Ljava/lang/Object;)Ljava/lang/String;"
        | "android.content.res.Resources.getQuantityText(II)Ljava/lang/CharSequence;" => {
            &[(0, &["plurals"])]
        }
        _ => return None,
    })
}

fn lex(chars: &[char]) -> Option<Vec<Token>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if result.len() >= 250_000 {
            return None;
        }
        if i == 0 || chars[i - 1] == '\n' {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |n| i + n);
            let line: String = chars[i..end].iter().collect();
            if line.trim_start().starts_with(".method ") {
                i = end;
                while i < chars.len() {
                    i += 1;
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == '\n')
                        .map_or(chars.len(), |n| i + n);
                    let terminal = chars[i..end].iter().collect::<String>();
                    i = end;
                    if terminal.trim() == ".end method" {
                        break;
                    }
                }
                continue;
            }
            if dex_directive(&line) {
                i = (end + 1).min(chars.len());
                continue;
            }
        }
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '/' && matches!(chars.get(i + 1), Some('/' | '*')) {
            let block = chars[i + 1] == '*';
            i += 2;
            while i < chars.len() {
                if !block && chars[i] == '\n' {
                    break;
                }
                if block && chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }
        let start = i;
        if matches!(chars[i], '"' | '\'') {
            let quote = chars[i];
            let triple =
                quote == '"' && chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"');
            i += if triple { 3 } else { 1 };
            while i < chars.len() {
                if chars[i] == '\\' {
                    i = (i + 2).min(chars.len());
                } else if chars[i] == quote
                    && (!triple
                        || (chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote)))
                {
                    i += if triple { 3 } else { 1 };
                    break;
                } else {
                    i += 1;
                }
            }
            result.push(Token {
                text: "#quoted".into(),
                span: start..i,
            });
            continue;
        }
        i += 1;
        if chars[start].is_alphanumeric() || matches!(chars[start], '_' | '$') {
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$')) {
                i += 1;
            }
        }
        result.push(Token {
            text: chars[start..i].iter().collect(),
            span: start..i,
        });
    }
    Some(result)
}

fn transparent_token(tokens: &[Token], mut range: Range<usize>) -> Option<usize> {
    for _ in 0..32 {
        let part = tokens.get(range.clone())?;
        if part.len() == 1 {
            return Some(range.start);
        }
        if part.len() >= 4 && part[0].text == "(" && part[1].text == "int" && part[2].text == ")" {
            range.start += 3;
            continue;
        }
        if part.first()?.text == "(" && part.last()?.text == ")" {
            let mut depth = 0;
            for (i, token) in part.iter().enumerate() {
                match token.text.as_str() {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    _ => {}
                }
                if depth == 0 && i + 1 < part.len() {
                    return None;
                }
            }
            range.start += 1;
            range.end -= 1;
            continue;
        }
        return None;
    }
    None
}

fn literal(tokens: &[Token], range: Range<usize>) -> Option<usize> {
    let token = transparent_token(tokens, range)?;
    tokens[token]
        .text
        .starts_with(|c: char| c.is_ascii_digit())
        .then_some(token)
}

/// Numeric token spans and the Android resource kinds proved at their uses.
pub(super) fn proven_arguments(
    code: &DecompiledCode,
    chars: &[char],
    classes: Option<&BTreeMap<String, crate::native_dex::DexClass>>,
) -> BTreeMap<(usize, usize), Vec<&'static str>> {
    let Some(tokens) = lex(chars) else {
        return BTreeMap::new();
    };
    let starts: BTreeMap<_, _> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (t.span.start, i))
        .collect();
    let definitions: BTreeSet<_> = code.definitions.iter().map(|d| (d.start, d.end)).collect();
    let mut result = BTreeMap::new();
    let mut work = 1_000_000usize;
    let mut resolver = super::contracts::Resolver::new(classes);
    for link in &code.links {
        let slots = resolver.resolve(&link.label);
        if slots.is_empty() {
            continue;
        }
        let Some(&name) = starts.get(&link.start) else {
            continue;
        };
        let raw_name = link
            .label
            .split('(')
            .next()
            .unwrap_or_default()
            .rsplit('.')
            .next()
            .unwrap_or_default();
        if tokens[name].span.end != link.end
            || !linked_name(&tokens[name].text, raw_name)
            || tokens.get(name + 1).map(|t| t.text.as_str()) != Some("(")
        {
            continue;
        }
        if definitions.contains(&(link.start, link.end)) {
            continue;
        }
        let mut stack = Vec::new();
        let mut begin = name + 2;
        let mut args = Vec::new();
        let mut closed = false;
        for (i, token) in tokens.iter().enumerate().skip(name + 2) {
            let Some(left) = work.checked_sub(1) else {
                return BTreeMap::new();
            };
            work = left;
            match token.text.as_str() {
                ")" if stack.is_empty() => {
                    args.push(begin..i);
                    closed = true;
                    break;
                }
                "," if stack.is_empty() => {
                    args.push(begin..i);
                    begin = i + 1;
                }
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                ")" | "]" | "}" => {
                    if stack.pop() != Some(token.text.as_str()) {
                        break;
                    }
                }
                ";" if stack.is_empty() => break,
                _ => {}
            }
        }
        if !closed || parameter_count(&link.label) != Some(args.len()) {
            continue;
        }
        for (slot, kinds) in slots {
            let Some(arg) = args.get(slot) else {
                continue;
            };
            let Some(left) = arg
                .len()
                .checked_mul(33)
                .and_then(|cost| work.checked_sub(cost))
            else {
                return BTreeMap::new();
            };
            work = left;
            let token = literal(&tokens, arg.clone())
                .or_else(|| single_use_local(&tokens, name, arg.clone(), &mut work));
            if let Some(token) = token {
                let span = &tokens[token].span;
                result
                    .entry((span.start, span.end))
                    .or_insert_with(Vec::new)
                    .extend(kinds);
            }
        }
    }
    result
}

// Native Java aliases encode the original member's UTF-8 bytes. The raw link
// remains the method identity; accepting its exact alias preserves that proof.
fn linked_name(display: &str, raw: &str) -> bool {
    if display == raw {
        return true;
    }
    let Some(hex) = display.strip_prefix("_rdx_") else {
        return false;
    };
    if hex.len() != raw.len() * 2 {
        return false;
    }
    hex.as_bytes()
        .chunks_exact(2)
        .zip(raw.bytes())
        .all(|(pair, byte)| {
            let digit = |v: u8| match v {
                b'0'..=b'9' => Some(v - b'0'),
                b'a'..=b'f' => Some(v - b'a' + 10),
                _ => None,
            };
            digit(pair[0])
                .zip(digit(pair[1]))
                .is_some_and(|(hi, lo)| hi * 16 + lo == byte)
        })
}

fn parameter_count(label: &str) -> Option<usize> {
    let mut chars = label.split_once('(')?.1.split_once(')')?.0.chars();
    let mut count = 0;
    while let Some(mut ch) = chars.next() {
        while ch == '[' {
            ch = chars.next()?;
        }
        match ch {
            'L' => {
                if !chars.by_ref().any(|ch| ch == ';') {
                    return None;
                }
            }
            'Z' | 'B' | 'C' | 'S' | 'I' | 'J' | 'F' | 'D' => {}
            _ => return None,
        }
        count += 1;
    }
    Some(count)
}

fn single_use_local(
    tokens: &[Token],
    method: usize,
    arg: Range<usize>,
    work: &mut usize,
) -> Option<usize> {
    // Charge the maximum extent of all five scans before following a local.
    *work = work.checked_sub(tokens.len().checked_mul(5)?)?;
    let token = transparent_token(tokens, arg)?;
    if !crate::native_java::identifier(&tokens[token].text) {
        return None;
    }
    let name = &tokens[token].text;
    // Restrict propagation to one immediately preceding int declaration and a
    // simple receiver call. No branch, intervening effect, reassignment or alias.
    let semi = (0..method)
        .rev()
        .find(|i| matches!(tokens[*i].text.as_str(), ";" | "{" | "}"))?;
    if tokens[semi].text != ";" {
        return None;
    }
    let receiver = &tokens[semi + 1..method];
    if receiver.iter().any(|t| {
        !matches!(t.text.as_str(), "." | "this" | "super")
            && !crate::native_java::identifier(&t.text)
    }) {
        return None;
    }
    let begin = (0..semi)
        .rev()
        .find(|i| matches!(tokens[*i].text.as_str(), ";" | "{" | "}"))?
        + 1;
    if tokens.get(begin)?.text != "int"
        || tokens.get(begin + 1)?.text != *name
        || tokens.get(begin + 2)?.text != "="
    {
        return None;
    }
    let value = literal(tokens, begin + 3..semi)?;
    // A single use over the enclosing lexical block ensures this declaration
    // never also supplies an unrelated hash/flag operation.
    let mut depth = 0usize;
    let mut block_start = None;
    for i in (0..begin).rev() {
        match tokens[i].text.as_str() {
            "}" => depth += 1,
            "{" if depth == 0 => {
                block_start = Some(i);
                break;
            }
            "{" => depth -= 1,
            _ => {}
        }
    }
    let block_start = block_start?;
    let mut end = tokens.len();
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(block_start + 1) {
        match token.text.as_str() {
            "{" => depth += 1,
            "}" if depth == 0 => {
                end = i;
                break;
            }
            "}" => depth -= 1,
            _ => {}
        }
    }
    if tokens[block_start..end]
        .iter()
        .filter(|t| t.text == *name)
        .count()
        != 2
    {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn method_link_accepts_only_its_exact_native_alias() {
        assert!(super::linked_name("setContentView", "setContentView"));
        assert!(super::linked_name("_rdx_d181", "с"));
        assert!(!super::linked_name("_rdx_d180", "с"));
        assert!(!super::linked_name("_rdx_zzzz", "с"));
        assert!(!super::linked_name("setText", "setContentView"));
    }
}
