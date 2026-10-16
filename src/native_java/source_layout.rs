//! Bounded whitespace-only layout for completed Java class presentations.

use crate::engine::{CodeDefinition, CodeLink, DecompiledCode};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_METADATA_ITEMS: usize = 262_144;
const MAX_CONSTRUCTORS: usize = 4096;
const MAX_PAREN_DEPTH: usize = 4096;
const MAX_EDITS: usize = 8192;
const MAX_INSERTED_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 120;
const MIN_CONSTRUCTOR_BYTES: usize = 80;

struct Constructor {
    start: usize,
    open: usize,
    close: usize,
    parent: Option<usize>,
    commas: Vec<usize>,
}

struct Frame {
    constructor: Option<usize>,
    commas: Vec<usize>,
}

struct ByteEdit {
    start: usize,
    end: usize,
    replacement: String,
}

struct Edit {
    byte_start: usize,
    byte_end: usize,
    char_start: usize,
    char_end: usize,
    replacement: String,
    replacement_chars: usize,
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn identifier_part(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn constructor_open(line: &str, at: usize) -> Option<usize> {
    if !line.is_char_boundary(at) {
        return None;
    }
    let bytes = line.as_bytes();
    if !line[at..].starts_with("new ")
        || at
            .checked_sub(1)
            .is_some_and(|previous| !bytes[previous].is_ascii() || identifier_part(bytes[previous]))
    {
        return None;
    }
    let mut cursor = at + "new ".len();
    loop {
        if !bytes
            .get(cursor)
            .is_some_and(|byte| identifier_start(*byte))
        {
            return None;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(|byte| identifier_part(*byte)) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'.') {
            break;
        }
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'(')).then_some(cursor)
}

// Method invocations use the same token-preserving argument layout. Declarations,
// casts and control headers are excluded by the enclosing statement guard.
fn invocation_open(line: &str, at: usize) -> Option<usize> {
    if !line.is_char_boundary(at) {
        return None;
    }
    let bytes = line.as_bytes();
    if !identifier_start(*bytes.get(at)?)
        || at
            .checked_sub(1)
            .is_some_and(|i| identifier_part(bytes[i]) || !bytes[i].is_ascii())
    {
        return None;
    }
    let mut end = at + 1;
    while bytes.get(end).is_some_and(|b| identifier_part(*b)) {
        end += 1;
    }
    if matches!(
        &line[at..end],
        "new" | "if" | "while" | "for" | "switch" | "catch" | "synchronized"
    ) {
        return None;
    }
    (bytes.get(end) == Some(&b'(')).then_some(end)
}

fn horizontal_after(line: &str, mut at: usize) -> usize {
    while line
        .as_bytes()
        .get(at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        at += 1;
    }
    at
}

fn horizontal_before(line: &str, mut at: usize) -> usize {
    while at > 0 && matches!(line.as_bytes()[at - 1], b' ' | b'\t') {
        at -= 1;
    }
    at
}

fn control_header(statement: &str) -> bool {
    [
        "if (",
        "for (",
        "while (",
        "switch (",
        "catch (",
        "synchronized (",
    ]
    .iter()
    .any(|prefix| statement.starts_with(prefix))
}

fn push_edit(
    edits: &mut Vec<ByteEdit>,
    inserted_bytes: &mut usize,
    start: usize,
    end: usize,
    replacement: String,
) -> Option<()> {
    if edits.len() == MAX_EDITS {
        return None;
    }
    *inserted_bytes = inserted_bytes.checked_add(replacement.len())?;
    if *inserted_bytes > MAX_INSERTED_BYTES {
        return None;
    }
    edits.push(ByteEdit {
        start,
        end,
        replacement,
    });
    Some(())
}

fn layout_line(
    line: &str,
    source_start: usize,
    edits: &mut Vec<ByteEdit>,
    inserted_bytes: &mut usize,
) -> Option<()> {
    let content = line.trim_end_matches(['\r', '\n']);
    if content.chars().count() <= MAX_LINE_CHARS || !content.ends_with(';') {
        return Some(());
    }
    let indent = content.len() - content.trim_start_matches(' ').len();
    let statement = &content[indent..];
    if indent < 8
        || statement.starts_with('@')
        || control_header(statement)
        || statement.contains("->")
        || statement
            .bytes()
            .any(|byte| matches!(byte, b'{' | b'}' | b'[' | b']' | b'?' | b'<' | b'>'))
    {
        return Some(());
    }

    let bytes = content.as_bytes();
    let mut constructors = Vec::<Constructor>::new();
    let mut opens = std::collections::HashMap::new();
    let mut stack = Vec::<Frame>::new();
    let mut active_constructors = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut semicolons = 0usize;
    let mut separators = 0usize;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            cursor += 1;
            continue;
        }
        if bytes
            .get(cursor..cursor + 2)
            .is_some_and(|pair| pair == b"//" || pair == b"/*" || pair == b"*/")
        {
            return Some(());
        }
        if let Some(open) =
            constructor_open(content, cursor).or_else(|| invocation_open(content, cursor))
            && !opens.contains_key(&open)
        {
            if constructors.len() == MAX_CONSTRUCTORS {
                return None;
            }
            let index = constructors.len();
            constructors.push(Constructor {
                start: cursor,
                open,
                close: usize::MAX,
                parent: None,
                commas: Vec::new(),
            });
            opens.insert(open, index);
        }
        match byte {
            b'(' => {
                if stack.len() == MAX_PAREN_DEPTH {
                    return None;
                }
                let constructor = opens.get(&cursor).copied();
                if let Some(index) = constructor {
                    constructors[index].parent = active_constructors.last().copied();
                    active_constructors.push(index);
                }
                stack.push(Frame {
                    constructor,
                    commas: Vec::new(),
                });
            }
            b',' => {
                if let Some(frame) = stack.last_mut()
                    && frame.constructor.is_some()
                {
                    separators += 1;
                    if separators > MAX_EDITS {
                        return None;
                    }
                    frame.commas.push(cursor);
                }
            }
            b')' => {
                let frame = stack.pop()?;
                if let Some(index) = frame.constructor {
                    if active_constructors.pop() != Some(index) {
                        return None;
                    }
                    constructors[index].close = cursor;
                    let mut boundaries = std::iter::once(constructors[index].open + 1)
                        .chain(frame.commas.iter().map(|comma| comma + 1))
                        .zip(frame.commas.iter().copied().chain(std::iter::once(cursor)));
                    if frame.commas.is_empty()
                        || boundaries.any(|(start, end)| content[start..end].trim().is_empty())
                    {
                        constructors[index].close = usize::MAX;
                    } else {
                        constructors[index].commas = frame.commas;
                    }
                }
            }
            b';' => semicolons += 1,
            _ => {}
        }
        cursor += 1;
    }
    if quote.is_some() || !stack.is_empty() || !active_constructors.is_empty() || semicolons != 1 {
        return Some(());
    }

    let mut selected = vec![false; constructors.len()];
    let mut depth = vec![0usize; constructors.len()];
    for (index, constructor) in constructors.iter().enumerate() {
        selected[index] = constructor.close != usize::MAX
            && constructor.close.saturating_sub(constructor.start) + 1 >= MIN_CONSTRUCTOR_BYTES;
        depth[index] = constructor
            .parent
            .map_or(0, |parent| depth[parent] + usize::from(selected[parent]));
    }
    if !selected.iter().any(|selected| *selected) {
        return Some(());
    }

    for (index, constructor) in constructors.iter().enumerate() {
        if !selected[index] {
            continue;
        }
        if edits
            .len()
            .checked_add(constructor.commas.len() + 2)
            .is_none_or(|total| total > MAX_EDITS)
        {
            return None;
        }
        let argument_indent = " ".repeat(indent + 4 * (depth[index] + 1));
        let closing_indent = " ".repeat(indent + 4 * depth[index]);
        let after_open = horizontal_after(content, constructor.open + 1);
        push_edit(
            edits,
            inserted_bytes,
            source_start + constructor.open + 1,
            source_start + after_open,
            format!("\n{argument_indent}"),
        )?;
        for &comma in &constructor.commas {
            let after = horizontal_after(content, comma + 1);
            push_edit(
                edits,
                inserted_bytes,
                source_start + comma + 1,
                source_start + after,
                format!("\n{argument_indent}"),
            )?;
        }
        let before_close = horizontal_before(content, constructor.close);
        push_edit(
            edits,
            inserted_bytes,
            source_start + before_close,
            source_start + constructor.close,
            format!("\n{closing_indent}"),
        )?;
    }
    Some(())
}

fn char_offsets(source: &str, edits: Vec<ByteEdit>) -> Option<Vec<Edit>> {
    let mut positions: Vec<_> = edits
        .iter()
        .flat_map(|edit| [edit.start, edit.end])
        .collect();
    positions.sort_unstable();
    positions.dedup();
    let mut mapped = std::collections::HashMap::with_capacity(positions.len());
    let mut next = 0usize;
    for (char_position, (byte_position, _)) in source.char_indices().enumerate() {
        while positions.get(next) == Some(&byte_position) {
            mapped.insert(byte_position, char_position);
            next += 1;
        }
    }
    while positions.get(next) == Some(&source.len()) {
        mapped.insert(source.len(), source.chars().count());
        next += 1;
    }
    if next != positions.len() {
        return None;
    }
    edits
        .into_iter()
        .map(|edit| {
            let char_start = *mapped.get(&edit.start)?;
            let char_end = *mapped.get(&edit.end)?;
            let replacement_chars = edit.replacement.chars().count();
            Some(Edit {
                byte_start: edit.start,
                byte_end: edit.end,
                char_start,
                char_end,
                replacement: edit.replacement,
                replacement_chars,
            })
        })
        .collect()
}

fn overlaps(start: usize, end: usize, edit: &Edit) -> bool {
    start < edit.char_end && end > edit.char_start
}

fn safe_metadata(links: &[CodeLink], definitions: &[CodeDefinition], edits: &[Edit]) -> bool {
    let mut spans: Vec<_> = links
        .iter()
        .map(|link| (link.start, link.end))
        .chain(
            definitions
                .iter()
                .map(|definition| (definition.start, definition.end)),
        )
        .collect();
    spans.sort_unstable();
    let mut edit = 0usize;
    for (start, end) in spans {
        while edit < edits.len() && edits[edit].char_end <= start {
            edit += 1;
        }
        if edits
            .get(edit)
            .is_some_and(|candidate| overlaps(start, end, candidate))
        {
            return false;
        }
    }
    true
}

fn remap(position: usize, edits: &[Edit], deltas: &[isize], after_insertions: bool) -> usize {
    let count = edits.partition_point(|edit| {
        edit.char_end < position
            || (edit.char_end == position && (edit.char_start != edit.char_end || after_insertions))
    });
    let delta = count.checked_sub(1).map_or(0, |index| deltas[index]);
    (position as isize + delta) as usize
}

fn apply(mut code: DecompiledCode, edits: Vec<ByteEdit>) -> DecompiledCode {
    if edits.is_empty() {
        return code;
    }
    let Some(mut edits) = char_offsets(&code.source, edits) else {
        return code;
    };
    edits.sort_by_key(|edit| (edit.byte_start, edit.byte_end));
    if edits.windows(2).any(|pair| {
        pair[0].byte_end > pair[1].byte_start || pair[0].byte_start == pair[1].byte_start
    }) || edits.iter().any(|edit| {
        !code.source[edit.byte_start..edit.byte_end]
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t'))
            || !edit.replacement.chars().all(char::is_whitespace)
    }) || !safe_metadata(&code.links, &code.definitions, &edits)
    {
        return code;
    }
    let mut delta = 0isize;
    let deltas: Vec<_> = edits
        .iter()
        .map(|edit| {
            delta += edit.replacement_chars as isize - (edit.char_end - edit.char_start) as isize;
            delta
        })
        .collect();
    for link in &mut code.links {
        link.start = remap(link.start, &edits, &deltas, true);
        link.end = remap(link.end, &edits, &deltas, false);
    }
    for definition in &mut code.definitions {
        definition.start = remap(definition.start, &edits, &deltas, true);
        definition.end = remap(definition.end, &edits, &deltas, false);
    }
    let mut source = String::with_capacity(
        code.source.len()
            + edits
                .iter()
                .map(|edit| {
                    edit.replacement
                        .len()
                        .saturating_sub(edit.byte_end - edit.byte_start)
                })
                .sum::<usize>(),
    );
    let mut cursor = 0usize;
    for edit in edits {
        source.push_str(&code.source[cursor..edit.byte_start]);
        source.push_str(&edit.replacement);
        cursor = edit.byte_end;
    }
    source.push_str(&code.source[cursor..]);
    code.source = source;
    code.source_hash = crate::engine::source_identity(&code.source);
    code
}

pub(super) fn format(code: DecompiledCode) -> DecompiledCode {
    if code.source.len() > MAX_SOURCE_BYTES
        || code.links.len().saturating_add(code.definitions.len()) > MAX_METADATA_ITEMS
        || code.source.contains("/*")
        || code.source.contains("\"\"\"")
    {
        return code;
    }
    let mut edits = Vec::new();
    let mut inserted_bytes = 0usize;
    let mut start = 0usize;
    let mut in_dex = false;
    for line in code.source.split_inclusive('\n') {
        if line.trim_start().starts_with(".method ") {
            in_dex = true;
        }
        if in_dex {
            if line.trim() == ".end method" {
                in_dex = false;
            }
            start += line.len();
            continue;
        }
        if layout_line(line, start, &mut edits, &mut inserted_bytes).is_none() {
            return code;
        }
        start += line.len();
    }
    apply(code, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeDefinition, CodeLink};

    #[test]
    fn mixed_source_formats_java_and_preserves_dex_verbatim() {
        let dex = ".method sample.Example.f()V\n    0000: const-string v0, \"service.combine(firstArgumentWithLongName, secondArgumentWithLongName, thirdArgumentWithLongName, fourthArgumentWithLongName);\"\n.end method\n";
        let java = "    void run() {\n        service.combine(firstArgumentWithLongName, secondArgumentWithLongName, thirdArgumentWithLongName, fourthArgumentWithLongName);\n    }\n";
        let source = format!("class Example {{\n{dex}{java}}}\n");
        let after = format(DecompiledCode {
            source: source.clone(),
            source_hash: crate::engine::source_identity(&source),
            links: vec![],
            definitions: vec![],
        });
        assert!(after.source.contains(dex));
        assert!(after.source.contains("service.combine(\n"));
        assert_eq!(compact_tokens(&source), compact_tokens(&after.source));
    }

    #[test]
    fn long_nested_method_calls_wrap_without_changing_tokens_or_navigation() {
        let source = "class Example {\n    void run() {\n        Object value = service.combine(firstArgumentWithLongName, helper.prepare(secondArgumentWithLongName, thirdArgumentWithLongName, \"λ, literal whitespace\"), fourthArgumentWithLongName);\n    }\n}\n";
        let start = source[..source.find("prepare").unwrap()].chars().count();
        let original = DecompiledCode {
            source: source.into(),
            source_hash: crate::engine::source_identity(source),
            links: vec![CodeLink {
                start,
                end: start + 7,
                label: "sample.Helper.prepare()Ljava/lang/Object;".into(),
            }],
            definitions: vec![],
        };
        let after = format(original);
        assert!(
            after.source.contains("service.combine(\n"),
            "{}",
            after.source
        );
        assert!(
            after.source.contains("helper.prepare(\n"),
            "{}",
            after.source
        );
        assert_eq!(compact_tokens(source), compact_tokens(&after.source));
        assert_eq!(
            after
                .source
                .chars()
                .skip(after.links[0].start)
                .take(7)
                .collect::<String>(),
            "prepare"
        );
        assert_eq!(format(after.clone()).source, after.source);
        assert_eq!(
            after.source_hash,
            crate::engine::source_identity(&after.source)
        );
    }

    fn code(source: &str) -> DecompiledCode {
        let class = source.find("Example").unwrap();
        let outer = source.find("Outer").unwrap();
        let inner = source.find("Inner").unwrap();
        let value = source.rfind("value").unwrap();
        let last = source.find("thirdArgumentWithLongName").unwrap();
        let scalar = |byte| source[..byte].chars().count();
        DecompiledCode {
            source: source.into(),
            source_hash: crate::engine::source_identity(source),
            links: vec![
                CodeLink {
                    start: scalar(outer),
                    end: scalar(outer + "Outer".len()),
                    label: "sample.Outer".into(),
                },
                CodeLink {
                    start: scalar(inner),
                    end: scalar(inner + "Inner".len()),
                    label: "sample.Inner".into(),
                },
                CodeLink {
                    start: scalar(value),
                    end: scalar(value + "value".len()),
                    label: "sample.Example.value:Ljava/lang/String;".into(),
                },
                CodeLink {
                    start: scalar(last),
                    end: scalar(last + "thirdArgumentWithLongName".len()),
                    label: "sample.Example.thirdArgumentWithLongName:Ljava/lang/String;".into(),
                },
            ],
            definitions: vec![CodeDefinition {
                start: scalar(class),
                end: scalar(class + "Example".len()),
                name: "Example".into(),
                kind: "class".into(),
            }],
        }
    }

    fn assert_unchanged(actual: &DecompiledCode, expected: &DecompiledCode) {
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.source_hash, expected.source_hash);
        assert_eq!(actual.links.len(), expected.links.len());
        for (actual, expected) in actual.links.iter().zip(&expected.links) {
            assert_eq!(
                (actual.start, actual.end, actual.label.as_str()),
                (expected.start, expected.end, expected.label.as_str())
            );
        }
        assert_eq!(actual.definitions.len(), expected.definitions.len());
        for (actual, expected) in actual.definitions.iter().zip(&expected.definitions) {
            assert_eq!(
                (
                    actual.start,
                    actual.end,
                    actual.kind.as_str(),
                    actual.name.as_str()
                ),
                (
                    expected.start,
                    expected.end,
                    expected.kind.as_str(),
                    expected.name.as_str()
                )
            );
        }
    }

    fn compact_tokens(source: &str) -> String {
        let mut result = String::new();
        let mut quote = None;
        let mut escaped = false;
        for character in source.chars() {
            if let Some(delimiter) = quote {
                result.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == delimiter {
                    quote = None;
                }
            } else if matches!(character, '\'' | '"') {
                quote = Some(character);
                result.push(character);
            } else if !character.is_whitespace() {
                result.push(character);
            }
        }
        result
    }

    #[test]
    fn long_nested_constructors_are_one_argument_per_line() {
        let before = concat!(
            "package sample;\n\n",
            "public class Example {\n",
            "    String value;\n",
            "    void create() {\n",
            "        Outer result = new Outer(((Contract) new Inner(value, call(\"x, é\"), secondArgumentWithLongName, thirdArgumentWithLongName)), ((State) null), 2, ((Marker) null));\n",
            "    }\n",
            "}\n",
        );
        let expected = concat!(
            "package sample;\n\n",
            "public class Example {\n",
            "    String value;\n",
            "    void create() {\n",
            "        Outer result = new Outer(\n",
            "            ((Contract) new Inner(\n",
            "                value,\n",
            "                call(\"x, é\"),\n",
            "                secondArgumentWithLongName,\n",
            "                thirdArgumentWithLongName\n",
            "            )),\n",
            "            ((State) null),\n",
            "            2,\n",
            "            ((Marker) null)\n",
            "        );\n",
            "    }\n",
            "}\n",
        );
        let original = code(before);
        let formatted = format(original.clone());
        assert_eq!(formatted.source, expected);
        assert_eq!(compact_tokens(&formatted.source), compact_tokens(before));
        assert_eq!(
            formatted.source_hash,
            crate::engine::source_identity(expected)
        );
        for (link, text) in
            formatted
                .links
                .iter()
                .zip(["Outer", "Inner", "value", "thirdArgumentWithLongName"])
        {
            assert_eq!(
                formatted
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                text
            );
        }
        let definition = &formatted.definitions[0];
        assert_eq!(
            formatted
                .source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            "Example"
        );
        let second = format(formatted.clone());
        assert_unchanged(&second, &formatted);
    }

    #[test]
    fn short_or_ambiguous_sources_remain_exact() {
        let long = "x".repeat(100);
        for source in [
            "        Result value = new Result(first, second);\n".to_owned(),
            format!("        @Example(value = new Outer(first, second, \"{long}\"));\n"),
            format!("        if (ready) new Outer(first, second, \"{long}\");\n"),
            format!("        Outer value = new Outer(first /* keep */, second, \"{long}\");\n"),
            format!(
                ".method public create()V\n        Outer value = new Outer(first, second, \"{long}\");\n.end method\n"
            ),
            format!("        Outer value = new Outer(first, second, \"{long}\";\n"),
            format!(
                "/* block\n        Outer value = new Outer(first, second, \"{long}\", fourth);\n*/\n"
            ),
            format!(
                "        String text = \"\"\"\n        Outer value = new Outer(first, second, {long}, fourth);\n        \"\"\";\n"
            ),
        ] {
            let input = DecompiledCode {
                source: source.clone(),
                source_hash: crate::engine::source_identity(&source),
                links: vec![],
                definitions: vec![],
            };
            let actual = format(input.clone());
            assert_unchanged(&actual, &input);
        }
    }

    #[test]
    fn metadata_crossing_replaced_whitespace_refuses_the_batch() {
        let long = "x".repeat(100);
        let source =
            format!("        Outer value = new Outer(first, second, \"{long}\", fourth);\n");
        let space = source.find("first, ").unwrap() + "first,".len();
        let input = DecompiledCode {
            source: source.clone(),
            source_hash: crate::engine::source_identity(&source),
            links: vec![CodeLink {
                start: space,
                end: space + 1,
                label: "ambiguous".into(),
            }],
            definitions: vec![],
        };
        let actual = format(input.clone());
        assert_unchanged(&actual, &input);
    }
}
