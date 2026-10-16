//! Remove proven reference upcasts without changing overloads or runtime checks.
//! Runs on emitter-owned, fully qualified Java before display names/imports.
use super::condition_cleanup::{Edit, Syntax, apply, syntax};
use crate::{
    engine::DecompiledCode,
    native_dex::{DexClass, DexMethod},
    native_hierarchy::Relation,
};
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 512 * 1024;
const MAX_WORK: usize = 2_000_000;

fn local(s: &str) -> bool {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('p'))
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

fn descriptor(ty: &str) -> Option<String> {
    let base = ty.trim_end_matches("[]");
    let dimensions = (ty.len() - base.len()) / 2;
    let primitive = match base {
        "boolean" => "Z",
        "byte" => "B",
        "short" => "S",
        "char" => "C",
        "int" => "I",
        "long" => "J",
        "float" => "F",
        "double" => "D",
        "void" => return None,
        _ => "",
    };
    if !primitive.is_empty() {
        return (dimensions > 0).then(|| format!("{}{primitive}", "[".repeat(dimensions)));
    }
    if base.is_empty() || !base.split('.').all(super::identifier) {
        return None;
    }
    Some(format!(
        "{}L{};",
        "[".repeat(dimensions),
        base.replace('.', "/")
    ))
}

fn types(
    source: &str,
    parsed: &Syntax,
    class: &DexClass,
    method: &DexMethod,
) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let mut duplicate = HashSet::new();
    let mut declared = HashSet::new();
    if method.access_flags & 8 == 0 {
        found.insert("this".into(), class.descriptor.to_string());
    }
    for (i, ty) in method.parameters.iter().enumerate() {
        declared.insert(format!("p{i}"));
        if ty.starts_with(['L', '[']) {
            found.insert(format!("p{i}"), ty.to_string());
        }
    }
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        if indent >= 8 && parsed.code.get(start + indent) == Some(&true) {
            let left = line
                .trim()
                .split_once(" = ")
                .map(|(left, _)| left)
                .or_else(|| line.trim().strip_suffix(';'));
            if let Some((ty, name)) = left.and_then(|s| s.split_once(' '))
                && local(name)
            {
                // A second declaration invalidates even a previously reference-typed local.
                if !declared.insert(name.to_owned()) {
                    duplicate.insert(name.to_owned());
                }
                if let Some(ty) = descriptor(ty) {
                    found.insert(name.into(), ty);
                }
            }
        }
        start += line.len();
    }
    found.retain(|name, _| !duplicate.contains(name));
    found
}

struct Cast<'a> {
    start: usize,
    end: usize,
    atom_start: usize,
    atom_end: usize,
    ty: &'a str,
    target: String,
    input: String,
}

fn cast<'a>(
    source: &'a str,
    parsed: &Syntax,
    start: usize,
    types: &HashMap<String, String>,
) -> Option<Cast<'a>> {
    if source.as_bytes().get(start..start + 2)? != b"((" || !parsed.code[start] {
        return None;
    }
    let type_end = *parsed.pair.get(start + 1)?;
    let close = *parsed.pair.get(start)?;
    if type_end == usize::MAX || close == usize::MAX || type_end >= close {
        return None;
    }
    let ty = source[start + 2..type_end].trim();
    let target = descriptor(ty)?;
    let raw = &source[type_end + 1..close];
    let atom = raw.trim();
    let atom_start = type_end + 1 + raw.len() - raw.trim_start().len();
    let atom_end = atom_start + atom.len();
    let input = if let Some(ty) = types.get(atom) {
        ty.clone()
    } else if atom == "null" {
        "null".into()
    } else if atom.starts_with('"')
        && atom.ends_with('"')
        && atom.len() >= 2
        && parsed.code[atom_start..atom_end].iter().all(|b| !b)
    {
        "Ljava/lang/String;".into()
    } else {
        return None;
    };
    Some(Cast {
        start,
        end: close + 1,
        atom_start,
        atom_end,
        ty,
        target,
        input,
    })
}

fn upcast(class: &DexClass, c: &Cast<'_>) -> bool {
    c.input == c.target
        || c.input == "null"
        || c.target == "Ljava/lang/Object;"
        || class
            .symbols
            .hierarchy
            .get()
            .is_some_and(|h| h.assignable(&c.input, &c.target) == Relation::Proven)
}

fn assignment(
    source: &str,
    c: &Cast<'_>,
    types: &HashMap<String, String>,
    class: &DexClass,
    method: &DexMethod,
) -> bool {
    let line_start = source[..c.start].rfind('\n').map_or(0, |p| p + 1);
    let line_end = source[c.end..]
        .find('\n')
        .map_or(source.len(), |p| c.end + p);
    if source[c.end..line_end].trim() != ";" {
        return false;
    }
    let prefix = source[line_start..c.start].trim();
    let destination = if prefix == "return" {
        Some(method.return_type.to_string())
    } else if let Some(lhs) = prefix.strip_suffix(" =") {
        if let Some((ty, _name)) = lhs.split_once(' ').filter(|(_, name)| local(name)) {
            descriptor(ty)
        } else if let Some(name) = lhs.strip_prefix("this.") {
            class
                .fields
                .iter()
                .find(|f| f.name.as_ref() == name)
                .map(|f| f.field_type.to_string())
        } else {
            types.get(lhs).cloned()
        }
    } else {
        None
    };
    destination.as_deref() == Some(&c.target)
}

struct Call {
    owner: String,
    name: String,
    args: Vec<String>,
    ret: String,
}
fn signature(label: &str) -> Option<Call> {
    let (head, tail) = label.split_once('(')?;
    let (owner, name) = head.rsplit_once('.')?;
    let (mut parameters, ret) = tail.split_once(')')?;
    let mut args = Vec::new();
    while !parameters.is_empty() {
        let array = parameters.bytes().take_while(|b| *b == b'[').count();
        let base = &parameters[array..];
        let len = if base.starts_with('L') {
            array + base.find(';')? + 1
        } else if base.starts_with(['Z', 'B', 'S', 'C', 'I', 'J', 'F', 'D']) {
            array + 1
        } else {
            return None;
        };
        args.push(parameters[..len].into());
        parameters = &parameters[len..];
        if args.len() > 255 {
            return None;
        }
    }
    Some(Call {
        owner: descriptor(owner)?,
        name: name.into(),
        args,
        ret: ret.into(),
    })
}

fn ranges(source: &str, parsed: &Syntax, open: usize) -> Option<Vec<(usize, usize)>> {
    let close = parsed.pair[open];
    if close == usize::MAX {
        return None;
    }
    let mut result = Vec::new();
    let mut start = open + 1;
    let mut i = start;
    while i < close {
        if parsed.code[i] {
            match source.as_bytes()[i] {
                b'(' | b'[' | b'{' => {
                    i = parsed.pair[i];
                    if i >= close {
                        return None;
                    }
                }
                b',' => {
                    result.push((start, i));
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start != close {
        result.push((start, close));
    }
    Some(
        result
            .into_iter()
            .map(|(a, b)| {
                let s = &source[a..b];
                let first = a + s.len() - s.trim_start().len();
                (first, first + s.trim().len())
            })
            .collect(),
    )
}

// Exact receiver type prevents a subclass-only overload from becoming visible.
fn receiver(
    source: &str,
    parsed: &Syntax,
    at: usize,
    call: &Call,
    types: &HashMap<String, String>,
) -> Option<bool> {
    let before = &source[..at];
    if call.name == "<init>" {
        return before.ends_with("new ").then_some(false);
    }
    let end = at.checked_sub(1)?;
    if source.as_bytes()[end] != b'.' {
        return None;
    }
    let mut start = end;
    while start > 0
        && (source.as_bytes()[start - 1].is_ascii_alphanumeric()
            || matches!(source.as_bytes()[start - 1], b'_' | b'$' | b'.'))
    {
        start -= 1;
    }
    if start == end && end > 0 && source.as_bytes()[end - 1] == b')' {
        start = parsed.pair[end - 1];
        let c = cast(source, parsed, start, types)?;
        return (c.end == end && c.target == call.owner).then_some(false);
    }
    let text = &source[start..end];
    if types.get(text).is_some_and(|ty| ty == &call.owner) {
        Some(false)
    } else if descriptor(text).as_deref() == Some(&call.owner) {
        Some(true)
    } else {
        None
    }
}

pub(super) fn simplify(
    code: DecompiledCode,
    class: &DexClass,
    method: &DexMethod,
) -> DecompiledCode {
    if method.code.is_none()
        || code.source.len() > MAX_BYTES
        || !code.source.contains("((")
        || code.source.contains("\\u")
    {
        return code;
    }
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    if !super::statement_cleanup::ordinary_method_scopes(&code.source, &parsed) {
        return code;
    }
    let types = types(&code.source, &parsed, class, method);
    let mut approved = HashSet::new();
    let mut work = 0usize;
    let mut char_bytes: Vec<_> = code.source.char_indices().map(|(b, _)| b).collect();
    char_bytes.push(code.source.len());
    if let Some(h) = class.symbols.hierarchy.get() {
        for link in &code.links {
            let Some(call) = signature(&link.label) else {
                continue;
            };
            let (Some(&begin), Some(&end)) = (char_bytes.get(link.start), char_bytes.get(link.end))
            else {
                return code;
            };
            if code.source.as_bytes().get(end) != Some(&b'(') || !parsed.code[end] {
                continue;
            }
            let Some(args) = ranges(&code.source, &parsed, end) else {
                continue;
            };
            work = work.saturating_add(code.source.len() + 1024);
            if work > MAX_WORK {
                return code;
            }
            if args.len() != call.args.len() {
                continue;
            }
            let Some(is_static) = receiver(&code.source, &parsed, begin, &call, &types) else {
                continue;
            };
            if !h.cast_cleanup_call(&call.owner, &call.name, &call.args, &call.ret, is_static) {
                continue;
            }
            for ((start, end), expected) in args.into_iter().zip(call.args) {
                if let Some(c) = cast(&code.source, &parsed, start, &types)
                    && c.end == end
                    && c.target == expected
                    && upcast(class, &c)
                {
                    approved.insert(start);
                }
            }
        }
    }
    let mut edits = Vec::new();
    let mut removed_types = Vec::new();
    for start in code
        .source
        .as_bytes()
        .windows(2)
        .enumerate()
        .filter_map(|(i, pair)| (pair == b"((").then_some(i))
    {
        work = work.saturating_add(code.source.len());
        if work > MAX_WORK {
            return code;
        }
        let Some(c) = cast(&code.source, &parsed, start, &types) else {
            continue;
        };
        if !upcast(class, &c)
            || !(c.input == c.target
                || approved.contains(&start)
                || assignment(&code.source, &c, &types, class, method))
        {
            continue;
        }
        work = work.saturating_add(code.links.len() + code.definitions.len());
        if work > MAX_WORK || edits.len() >= 2048 {
            return code;
        }
        // Only the eliminated type's navigation link may disappear.
        let type_start = c.start + 2;
        removed_types.push((type_start, type_start + c.ty.len(), c.ty));
        edits.push(Edit {
            start: c.start,
            end: c.atom_start,
            text: "",
        });
        edits.push(Edit {
            start: c.atom_end,
            end: c.end,
            text: "",
        });
    }
    if edits.is_empty() {
        return code;
    }
    let mut candidate = code.clone();
    candidate.links.retain(|link| {
        !removed_types.iter().any(|&(a, b, ty)| {
            char_bytes.get(link.start) == Some(&a)
                && char_bytes.get(link.end) == Some(&b)
                && link.label == ty
        })
    });
    let (updated, changed) = apply(candidate, edits);
    if changed { updated } else { code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{CodeLink, source_identity},
        native_dex::{DexCode, DexSymbols},
        native_hierarchy::TypeHierarchy,
    };
    use std::sync::Arc;

    fn method(owner: &str, name: &str, args: &[&str]) -> DexMethod {
        DexMethod {
            declaring_type: owner.into(),
            name: name.into(),
            parameters: args.iter().map(|s| Arc::from(*s)).collect(),
            return_type: "Ljava/lang/Object;".into(),
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 3,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 0,
            }),
        }
    }
    fn class(owner: &str) -> DexClass {
        DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: owner.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        }
    }
    fn target() -> DexClass {
        let mut c = class("Lsample/Target;");
        c.methods.push(method(
            &c.descriptor,
            "m",
            &[
                "Ljava/lang/Object;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ));
        c
    }
    fn run(source: &str, target: DexClass, extra: &[DexClass]) -> DecompiledCode {
        let caller = class("Lsample/Caller;");
        let h = TypeHierarchy::from_classes([&caller, &target].into_iter().chain(extra)).unwrap();
        caller.symbols.hierarchy.set(Arc::new(h)).unwrap();
        let mut code = DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            links: vec![],
            definitions: vec![],
        };
        for (at, _) in source.match_indices(".m(") {
            let start = source[..at + 1].chars().count();
            code.links.push(CodeLink { start, end: start + 1, label: "sample.Target.m(Ljava/lang/Object;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/Object;".into() });
        }
        simplify(code, &caller, &method("Lsample/Caller;", "test", &[]))
    }
    const CALL: &str = "    public Object test() {\n        sample.Target v0 = null;\n        v0.m(((java.lang.Object) \"1\"), \"$setOnce\", \"BETA\");\n        return null;\n    }\n";

    #[test]
    fn removes_life360_shaped_argument_with_loaded_interfaces() {
        let mut t = target();
        t.interfaces.push("Lsample/Callback;".into());
        let mut callback = class("Lsample/Callback;");
        callback.access_flags = 0x601;
        let out = run(CALL, t, &[callback]);
        assert!(
            out.source.contains("v0.m(\"1\", \"$setOnce\", \"BETA\")"),
            "{}",
            out.source
        );
        assert_eq!(out.source_hash, source_identity(&out.source));
        for link in out.links {
            assert_eq!(
                out.source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                "m"
            );
        }
    }
    #[test]
    fn retains_overload_varargs_unknown_parent_and_inherited_candidates() {
        let mut overloaded = target();
        overloaded.methods.push(method(
            "Lsample/Target;",
            "m",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ));
        assert_eq!(run(CALL, overloaded, &[]).source, CALL);
        let mut varargs = target();
        let mut other = method("Lsample/Target;", "m", &["[Ljava/lang/Object;"]);
        other.access_flags |= 0x80;
        varargs.methods.push(other);
        assert_eq!(run(CALL, varargs, &[]).source, CALL);
        let mut unknown = target();
        unknown.superclass = Some("Lmissing/Base;".into());
        assert_eq!(run(CALL, unknown, &[]).source, CALL);
        let mut inherited = target();
        inherited.superclass = Some("Lsample/Base;".into());
        let mut base = class("Lsample/Base;");
        base.methods.push(method(
            "Lsample/Base;",
            "m",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ));
        assert_eq!(run(CALL, inherited, &[base]).source, CALL);
        let mut bridge = target();
        bridge.methods[0].access_flags |= 0x40;
        assert_eq!(run(CALL, bridge, &[]).source, CALL);
    }
    #[test]
    fn removes_assignment_return_identity_and_keeps_runtime_checks() {
        let source = "    public Object test() {\n        java.lang.String v1 = \"α🙂\";\n        java.lang.Object v2 = ((java.lang.Object) v1);\n        java.lang.String v3 = ((java.lang.String) v1);\n        java.lang.String v4 = ((java.lang.String) v2);\n        int v5 = ((int) 1L);\n        return ((java.lang.Object) v1);\n    }\n";
        let out = run(source, target(), &[]).source;
        assert!(out.contains("java.lang.Object v2 = v1;"));
        assert!(out.contains("java.lang.String v3 = v1;"));
        assert!(out.contains("v4 = ((java.lang.String) v2);"));
        assert!(out.contains("v5 = ((int) 1L);"));
        assert!(out.contains("return v1;"));
    }
    #[test]
    fn preserves_duplicate_locals_comments_and_unknown_receivers() {
        let source = "    public Object test() {\n        int v1 = 1;\n        java.lang.String v1 = null;\n        return ((java.lang.Object) v1);\n    }\n";
        assert_eq!(run(source, target(), &[]).source, source);
        let source = CALL.replace("sample.Target v0", "sample.Sub v0");
        assert_eq!(run(&source, target(), &[]).source, source);
        let source = "    public Object test() {\n        // return ((java.lang.Object) \"1\");\n        return \"((java.lang.Object) x)\";\n    }\n";
        assert_eq!(run(source, target(), &[]).source, source);
    }
    #[test]
    fn keeps_generic_annotated_and_missing_declarations() {
        use crate::native_dex::{DexAnnotation, DexAnnotationDirectory};
        for generic_class in [false, true] {
            let mut t = target();
            let annotation: Arc<[Arc<DexAnnotation>]> = vec![Arc::new(DexAnnotation {
                visibility: 2,
                type_idx: 0,
                elements: vec![],
            })]
            .into();
            t.annotations_offset = 1;
            let mut symbols = DexSymbols {
                types: vec!["Ldalvik/annotation/Signature;".into()],
                ..Default::default()
            };
            symbols.annotations.insert(
                1,
                Arc::new(DexAnnotationDirectory {
                    class: generic_class.then(|| annotation.clone()),
                    methods: if generic_class {
                        vec![]
                    } else {
                        vec![Some(annotation)]
                    },
                    ..Default::default()
                }),
            );
            t.symbols = Arc::new(symbols);
            assert_eq!(run(CALL, t, &[]).source, CALL);
        }
        let mut missing = target();
        missing.annotations_offset = 42;
        assert_eq!(run(CALL, missing, &[]).source, CALL);
        let null = CALL.replace("\"1\"", "null");
        assert!(run(&null, target(), &[]).source.contains("v0.m(null,"));
        let mut overloaded = target();
        overloaded.methods.push(method(
            "Lsample/Target;",
            "m",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ));
        assert_eq!(run(&null, overloaded, &[]).source, null);
    }

    #[test]
    fn typed_interface_upcast_and_metadata_survive_unicode() {
        let mut caller = class("Lsample/Caller;");
        caller
            .methods
            .push(method("Lsample/Caller;", "test", &["Lsample/Callback;"]));
        caller.methods[0].return_type = "Lsample/Function;".into();
        let mut callback = class("Lsample/Callback;");
        callback.interfaces.push("Lsample/Function;".into());
        let interface = class("Lsample/Function;");
        caller
            .symbols
            .hierarchy
            .set(Arc::new(
                TypeHierarchy::from_classes([&caller, &callback, &interface]).unwrap(),
            ))
            .unwrap();
        let source = "    public sample.Function test(sample.Callback p0) {\n        // α🙂\n        return ((sample.Function) p0);\n    }\n";
        let type_start = source[..source.rfind("sample.Function").unwrap()]
            .chars()
            .count();
        let atom_start = source[..source.rfind("p0").unwrap()].chars().count();
        let code = DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            definitions: vec![],
            links: vec![
                CodeLink {
                    start: type_start,
                    end: type_start + 15,
                    label: "sample.Function".into(),
                },
                CodeLink {
                    start: atom_start,
                    end: atom_start + 2,
                    label: "parameter".into(),
                },
            ],
        };
        let out = simplify(code.clone(), &caller, &caller.methods[0]);
        assert!(out.source.contains("return p0;"), "{}", out.source);
        assert_eq!(out.links.len(), 1);
        assert_eq!(
            out.source
                .chars()
                .skip(out.links[0].start)
                .take(2)
                .collect::<String>(),
            "p0"
        );
        let mut overlapping = code;
        overlapping.links[0].label = "unknown navigation".into();
        assert_eq!(
            simplify(overlapping, &caller, &caller.methods[0]).source,
            source
        );
    }

    #[test]
    fn jvm_original_and_cleaned_preserve_values_and_overload_choice() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!(
            "rdx-cast-jvm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let unique = run(CALL, target(), &[])
            .source
            .replace("sample.Target", "Target");
        let mut overloaded = target();
        overloaded.methods.push(method(
            "Lsample/Target;",
            "m",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ));
        let retained = run(CALL, overloaded, &[])
            .source
            .replace("sample.Target", "Target");
        for (i, body) in [CALL.replace("sample.Target", "Target"), unique, retained]
            .into_iter()
            .enumerate()
        {
            let body = body.replace("Target v0 = null", "Target v0 = new Target()");
            let overload = if i == 2 {
                "public Object m(String a, String b, String c) { System.out.print(\"WRONG\"); return null; }"
            } else {
                ""
            };
            let java = format!(
                "public class Probe {{ static class Target {{ public Object m(Object a,String b,String c) {{ System.out.print(a.toString()+b+c); return null; }} {overload} }} {body} public static void main(String[] args) {{ new Probe().test(); }} }}"
            );
            let path = dir.join("Probe.java");
            std::fs::write(&path, java).unwrap();
            let compiled = Command::new("javac")
                .arg(&path)
                .output()
                .expect("javac required for semantic regression");
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let executed = Command::new("java")
                .arg("-cp")
                .arg(&dir)
                .arg("Probe")
                .output()
                .unwrap();
            assert!(executed.status.success());
            assert_eq!(executed.stdout, b"1$setOnceBETA");
        }
    }
}
