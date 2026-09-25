//! Same-header conditional and unconditional continues carry iteration state.
use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::{collections::HashMap, sync::Arc};
fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols::default());
    let method = &mut class.methods[0];
    method.parameters = vec!["I".into()];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.outs = 0;
    // Increment index; continue on even index; add odd index; continue at3;
    // otherwise add10. All three backedges share pc2, exit is pc18.
    code.instructions = vec![
        0x0012, 0x0112, 0x4035, 16, 0x00d8, 0x0100, 0x02dd, 0x0100, 0x0238, 0xfffa, 0x01b0, 0x3212,
        0x2033, 3, 0xf428, 0x01d8, 0x0a01, 0xf128, 0x010f,
    ];
    class
}
#[derive(Debug, PartialEq)]
enum Flow {
    Next,
    Break,
    Continue,
    Return(i32),
}
fn expression(mut text: &str, vars: &HashMap<String, i32>) -> i32 {
    text = text.trim();
    while text.starts_with('(') && text.ends_with(')') {
        let mut depth = 0;
        let mut whole = true;
        for (i, c) in text.char_indices() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            if depth == 0 && i + 1 < text.len() {
                whole = false;
                break;
            }
        }
        if !whole {
            break;
        }
        text = text[1..text.len() - 1].trim();
    }
    for op in [" >= ", " <= ", " == ", " != ", " > ", " < ", " + ", " & "] {
        let mut depth = 0;
        for (i, c) in text.char_indices() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            if depth == 0 && text[i..].starts_with(op) {
                let a = expression(&text[..i], vars);
                let b = expression(&text[i + op.len()..], vars);
                return match op {
                    " >= " => i32::from(a >= b),
                    " <= " => i32::from(a <= b),
                    " == " => i32::from(a == b),
                    " != " => i32::from(a != b),
                    " > " => i32::from(a > b),
                    " < " => i32::from(a < b),
                    " + " => a.wrapping_add(b),
                    " & " => a & b,
                    _ => unreachable!(),
                };
            }
        }
    }
    if let Some(value) = text.strip_prefix('!') {
        return i32::from(expression(value, vars) == 0);
    }
    if text == "true" {
        return 1;
    }
    if text == "false" {
        return 0;
    }
    if let Ok(n) = text.parse() {
        return n;
    }
    *vars
        .get(text)
        .unwrap_or_else(|| panic!("undefined expression {text}"))
}
fn close(lines: &[&str], start: usize) -> usize {
    let mut depth = 0;
    for (i, line) in lines.iter().enumerate().skip(start) {
        if line.starts_with('}') {
            depth -= 1;
            if depth == 0 && *line == "}" {
                return i;
            }
        }
        if line.ends_with('{') {
            depth += 1;
        }
    }
    panic!("unclosed block")
}
fn block(lines: &[&str], vars: &mut HashMap<String, i32>, budget: &mut usize) -> Flow {
    let mut pc = 0;
    while pc < lines.len() {
        assert!(*budget > 0, "execution exceeded bounded budget");
        *budget -= 1;
        let line = lines[pc];
        if line == "while (true) {" {
            let end = close(lines, pc);
            loop {
                match block(&lines[pc + 1..end], vars, budget) {
                    Flow::Break => break,
                    Flow::Return(value) => return Flow::Return(value),
                    Flow::Next | Flow::Continue => {}
                }
            }
            pc = end + 1;
            continue;
        }
        if let Some(test) = line
            .strip_prefix("if (")
            .and_then(|s| s.strip_suffix(") {"))
        {
            let end = close(lines, pc);
            let mut depth = 0;
            let mut otherwise = None;
            for (offset, nested) in lines[pc + 1..end].iter().enumerate() {
                if *nested == "} else {" && depth == 0 {
                    otherwise = Some(pc + 1 + offset);
                    break;
                }
                if nested.starts_with('}') {
                    depth -= 1;
                }
                if nested.ends_with('{') {
                    depth += 1;
                }
            }
            let flow = if expression(test, vars) != 0 {
                block(&lines[pc + 1..otherwise.unwrap_or(end)], vars, budget)
            } else if let Some(otherwise) = otherwise {
                block(&lines[otherwise + 1..end], vars, budget)
            } else {
                Flow::Next
            };
            if flow != Flow::Next {
                return flow;
            }
            pc = end + 1;
            continue;
        }
        match line {
            "break;" => return Flow::Break,
            "continue;" => return Flow::Continue,
            _ => {
                if let Some(value) = line
                    .strip_prefix("return ")
                    .and_then(|s| s.strip_suffix(';'))
                {
                    return Flow::Return(expression(value, vars));
                }
                if let Some((left, right)) =
                    line.strip_suffix(';').and_then(|s| s.split_once(" = "))
                {
                    let name = left.split_whitespace().last().unwrap();
                    let value = expression(right, vars);
                    vars.insert(name.into(), value);
                } else {
                    assert!(
                        line.starts_with("int ") && line.ends_with(';'),
                        "unexpected Java statement {line}"
                    );
                }
            }
        }
        pc += 1;
    }
    Flow::Next
}
#[test]
fn all_continue_paths_match_independent_iteration_results() {
    let class = fixture();
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches("while (true)").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("continue;").count(),
        2,
        "{}",
        code.source
    );
    let lines: Vec<_> = code.source.lines().map(str::trim).collect();
    for n in -2..=24 {
        let mut sum = 0;
        for i in 1..=n {
            if i % 2 == 0 {
                continue;
            }
            sum += i;
            if i == 3 {
                continue;
            }
            sum += 10;
        }
        assert_eq!(
            block(
                &lines[1..lines.len() - 1],
                &mut HashMap::from([("p0".into(), n)]),
                &mut 20_000
            ),
            Flow::Return(sum),
            "n={n}: {}",
            code.source
        );
    }
}
#[test]
fn outside_branch_into_iteration_and_different_header_cycles_still_reject() {
    for words in [
        vec![0x0228, 0, 0x003c, 0xffff, 0x000e],
        vec![0, 0, 0x003c, 0xfffe, 0x003c, 0xfffd, 0x000e],
    ] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions = words;
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn loop_can_return_through_shared_external_void_tail() {
    for words in [
        vec![
            0x0012, 0x1035, 7, 0x00d8, 0x0100, 0x1032, 7, 0xfa28, 0x0071, 0, 0, 0x000e, 0x000e,
        ],
        vec![
            0x0012, 0x1035, 8, 0x00d8, 0x0100, 0x1033, 3, 0x0628, 0xf928, 0x0071, 0, 0, 0x000e,
            0x000e,
        ],
    ] {
        let mut class = fixture();
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["touch".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let method = &mut class.methods[0];
        method.return_type = "V".into();
        let code = method.code.as_mut().unwrap();
        code.registers = 2;
        code.instructions = words;
        let result = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
        let source = &result.source;
        let start = source.find("while (").unwrap();
        let early = source[start..].find("return;").unwrap() + start;
        let effect = source.find("sample.Effects.touch()").unwrap();
        assert!(early < effect, "{source}");
        assert_eq!(source.matches("sample.Effects.touch()").count(), 1);
        // An external effectful path must not be replaced with a bare return.
        let code = class.methods[0].code.as_mut().unwrap();
        let end = code.instructions.len() - 1;
        code.instructions.splice(end.., [0x0071, 0, 0, 0x000e]);
        let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .unwrap()
            .source;
        // Both complete terminal tails are now reconstructed. Each path keeps
        // its effect instead of replacing that path with a bare return.
        assert_eq!(
            source.matches("sample.Effects.touch()").count(),
            2,
            "{source}"
        );
    }
}

#[test]
fn unchanged_zero_in_loop_retains_null_interpretation() {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["consume".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("V".into(), vec!["Ljava/lang/Object;".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0212, 0x3035, 8, 0x1071, 0, 2, 0x00d8, 0x0100, 0xf928, 0x000e,
    ];
    let result = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(
        result.source.contains("consume(((java.lang.Object) null))"),
        "{}",
        result.source
    );
    // A mutated numeric register must not retain the original null constant.
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0012, 0x0212, 0x3035, 9, 0x1071, 0, 2, 0x1212, 0x00d8, 0x0100, 0xf828, 0x000e,
    ];
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn backward_conditional_to_acyclic_shared_tail_preserves_results() {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.parameters = vec!["I".into(), "I".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.instructions = vec![0x0138, 4, 0x7012, 0x0528, 0x023a, 0xfffe, 0x0013, 9, 0x000f];
    let result = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    let lines: Vec<_> = result.source.lines().map(str::trim).collect();
    for flag in [-1, 0, 1] {
        for value in [-3, 0, 5] {
            let expected = if flag != 0 || value < 0 { 7 } else { 9 };
            assert_eq!(
                block(
                    &lines[1..lines.len() - 1],
                    &mut HashMap::from([("p0".into(), flag), ("p1".into(), value)]),
                    &mut 1000
                ),
                Flow::Return(expected),
                "{}",
                result.source
            );
        }
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn computed_external_loop_tails_execute_their_effect_once() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-loop-effect-tails-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut source = String::from(
        "public class LoopEffects { static class Effects { static int count; static void touch(){count++;} }\n",
    );
    for (i, words) in [
        vec![
            0x0012, 0x1035, 7, 0x00d8, 0x0100, 0x1032, 7, 0xfa28, 0x0071, 0, 0, 0x000e, 0x0071, 0,
            0, 0x000e,
        ],
        vec![
            0x0012, 0x1035, 8, 0x00d8, 0x0100, 0x1033, 3, 0x0628, 0xf928, 0x0071, 0, 0, 0x000e,
            0x0071, 0, 0, 0x000e,
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let mut class = fixture();
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["touch".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let m = &mut class.methods[0];
        m.name = format!("path{i}").into();
        m.return_type = "V".into();
        let code = m.code.as_mut().unwrap();
        code.registers = 2;
        code.instructions = words;
        source.push_str(
            &native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source
                .replace("sample.Effects", "Effects"),
        );
    }
    source.push_str("public static void main(String[] args){for(int n=-3;n<10;n++){Effects.count=0;path0(n);if(Effects.count!=1)throw new AssertionError(\"path0\");Effects.count=0;path1(n);if(Effects.count!=1)throw new AssertionError(\"path1\");}}}\n");
    fs::write(dir.join("LoopEffects.java"), &source).unwrap();
    for (program, arg) in [("javac", "LoopEffects.java"), ("java", "LoopEffects")] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
