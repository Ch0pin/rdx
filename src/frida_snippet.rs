//! Clipboard-only tracing snippets from exact DEX method identities.
//! Format informed by JADX's FridaAction; no script is executed by RDX.

pub fn for_method(label: &str) -> Option<String> {
    let (qualified, signature) = label.split_once('(')?;
    let (owner, name) = qualified.rsplit_once('.')?;
    if owner.is_empty() || name.is_empty() || name == "<clinit>" || owner.contains([':', '/', ';'])
    {
        return None;
    }
    let (arguments, result) = signature.split_once(')')?;
    let mut rest = arguments;
    let mut types = Vec::new();
    while !rest.is_empty() {
        let (ty, remaining) = descriptor(rest, false)?;
        types.push(ty);
        rest = remaining;
        if types.len() > 255 {
            return None;
        }
    }
    let (_, remaining) = descriptor(result, true)?;
    if !remaining.is_empty() || (name == "<init>" && result != "V") {
        return None;
    }
    if name.starts_with('<') && name != "<init>" {
        return None;
    }
    let method = if name == "<init>" { "$init" } else { name };
    let short = owner.rsplit(['.', '$']).next()?;
    let variable = if identifier(short) {
        short
    } else {
        "TargetClass"
    };
    let quote = |s: &str| serde_json::to_string(s).unwrap();
    let args: Vec<_> = (0..types.len()).map(|i| format!("arg{i}")).collect();
    let arg_list = args.join(", ");
    let overload = types
        .iter()
        .map(|ty| quote(ty))
        .collect::<Vec<_>>()
        .join(", ");
    let call_args = if args.is_empty() {
        "this".into()
    } else {
        format!("this, {arg_list}")
    };
    let log_name = format!("{short}.{method}");
    let mut log = quote(&format!("{log_name} is called"));
    for (i, arg) in args.iter().enumerate() {
        log.push_str(&format!(
            " + {} + {arg}",
            quote(&format!("{}{}=", if i == 0 { ": " } else { ", " }, arg))
        ));
    }
    let mut out = format!(
        "var {variable} = Java.use({});\nvar $rdxMethod = {variable}[{}].overload({overload});\n$rdxMethod.implementation = function ({arg_list}) {{\n    console.log({log});\n",
        quote(owner),
        quote(method)
    );
    if result == "V" {
        out.push_str(&format!("    $rdxMethod.call({call_args});\n"));
    } else {
        out.push_str(&format!("    let result = $rdxMethod.call({call_args});\n    console.log({} + result);\n    return result;\n", quote(&format!("{log_name} result="))));
    }
    out.push_str("};");
    let body = out
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("Java.perform(function () {{\n{body}\n}});"))
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            c.is_ascii_alphabetic() || c == '_' || c == '$' || (i > 0 && c.is_ascii_digit())
        })
        && !matches!(
            name,
            "$rdxMethod"
                | "Java"
                | "console"
                | "arguments"
                | "eval"
                | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "implements"
                | "import"
                | "in"
                | "instanceof"
                | "interface"
                | "let"
                | "new"
                | "null"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "static"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        )
}

fn descriptor(input: &str, allow_void: bool) -> Option<(String, &str)> {
    let dimensions = input.bytes().take_while(|b| *b == b'[').count();
    if dimensions > 255 {
        return None;
    }
    let tail = &input[dimensions..];
    let first = *tail.as_bytes().first()?;
    let (base, width) = match first {
        b'V' if allow_void && dimensions == 0 => ("void".to_owned(), 1),
        b'Z' => ("boolean".into(), 1),
        b'B' => ("byte".into(), 1),
        b'C' => ("char".into(), 1),
        b'S' => ("short".into(), 1),
        b'I' => ("int".into(), 1),
        b'J' => ("long".into(), 1),
        b'F' => ("float".into(), 1),
        b'D' => ("double".into(), 1),
        b'L' => {
            let end = tail.find(';')?;
            let raw = &tail[1..end];
            if raw.is_empty()
                || raw.contains(['[', '(', ')', '.'])
                || raw.split('/').any(str::is_empty)
            {
                return None;
            }
            (raw.replace('/', "."), end + 1)
        }
        _ => return None,
    };
    let consumed = dimensions + width;
    let ty = if dimensions == 0 {
        base
    } else {
        input[..consumed].replace('/', ".")
    };
    Some((ty, &input[consumed..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_overload_arrays_nested_class_and_return() {
        let s =
            for_method("app.Outer$Client.load(Landroid/webkit/WebView;[[I[Ljava/lang/String;)Z")
                .unwrap();
        assert!(s.contains("Java.use(\"app.Outer$Client\")"));
        assert!(
            s.contains(".overload(\"android.webkit.WebView\", \"[[I\", \"[Ljava.lang.String;\")")
        );
        assert_eq!(
            s.matches("$rdxMethod.call(this, arg0, arg1, arg2)").count(),
            1
        );
        assert!(s.contains("return result;"));
    }
    #[test]
    fn constructor_void_and_no_args() {
        let s = for_method("app.Widget.<init>()V").unwrap();
        assert!(s.contains("[\"$init\"].overload()"));
        assert!(s.contains("$rdxMethod.call(this);"));
        assert!(!s.contains("return result"));
        assert!(
            for_method("app.Widget.run()V")
                .unwrap()
                .contains("function ()")
        );
    }
    #[test]
    fn invalid_symbols_and_initializers_are_disabled() {
        for label in [
            "app.Widget",
            "app.Widget.value:I",
            "app.Widget.<clinit>()V",
            "app.Widget.f(V)V",
            "app.Widget.f([V)V",
            "app.Widget.f(L;)V",
            "app.Widget.f()VI",
            "app.Widget.<init>()I",
        ] {
            assert!(for_method(label).is_none(), "{label}");
        }
    }
    #[test]
    fn names_are_quoted_and_identifiers_safe() {
        let s = for_method("app.class.weird\"name()I").unwrap();
        assert!(s.contains("var TargetClass ="));
        assert!(s.contains("[\"weird\\\"name\"]"));
        let s = for_method("app.console.run()V").unwrap();
        assert!(s.contains("var TargetClass ="));
    }
}
