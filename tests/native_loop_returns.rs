//! Regression for a descending activity-stack scan with an interior return.
use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["count".into(), "at".into()],
        types: vec![
            "Lsample/Stack;".into(),
            "Lsample/Activity;".into(),
            "Lsample/IMActivity;".into(),
        ],
        protos: vec![
            ("I".into(), vec![]),
            ("Lsample/Activity;".into(), vec!["I".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = "getIMActivity".into();
    m.access_flags = 2;
    m.parameters.clear();
    m.return_type = "Lsample/Activity;".into();
    let c = m.code.as_mut().unwrap();
    c.registers = 4;
    c.ins = 1;
    c.outs = 1;
    c.instructions = vec![
        0x0071, 0, 0, 0x000a, 0x003d, 0x0012, 0x00d8, 0xff00, 0x003a, 0x000e, 0x1071, 1, 0, 0x010c,
        0x1220, 2, 0x0238, 3, 0x0111, 0x00d8, 0xff00, 0xf328, 0x0012, 0x0011,
    ];
    class
}

#[test]
fn loop_with_interior_reference_return_reconstructs() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(
        source.matches("sample.Stack.count()").count(),
        1,
        "{source}"
    );
    assert_eq!(source.matches("sample.Stack.at(").count(), 1, "{source}");
    assert!(source.contains("while (true)"), "{source}");
    assert!(source.contains("instanceof sample.IMActivity"), "{source}");
    assert!(source.contains("return null;"), "{source}");
}

#[derive(Default)]
struct Calls {
    count: usize,
    indexes: Vec<usize>,
}

// Independent, bounded execution of the displayed Java subset. Object identities
// are index+1; zero is null. Calls are recorded to verify order and short circuit.
fn execute_java(source: &str, stack: &[Option<bool>]) -> (i32, Calls) {
    use std::collections::HashMap;
    enum Flow {
        Next,
        Break,
        Return(i32),
    }
    fn expr(
        s: &str,
        vars: &HashMap<String, i32>,
        stack: &[Option<bool>],
        calls: &mut Calls,
    ) -> i32 {
        let mut s = s.trim();
        while s.starts_with('(') && s.ends_with(')') {
            let mut depth = 0;
            let wraps = s.char_indices().all(|(i, c)| {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                }
                depth != 0 || i == s.len() - 1
            });
            if !wraps {
                break;
            }
            s = &s[1..s.len() - 1];
        }
        for op in [" == ", " != ", " < ", " > ", " + "] {
            if let Some((a, b)) = s.split_once(op) {
                let a = expr(a, vars, stack, calls);
                let b = expr(b, vars, stack, calls);
                return match op {
                    " == " => i32::from(a == b),
                    " != " => i32::from(a != b),
                    " < " => i32::from(a < b),
                    " > " => i32::from(a > b),
                    " + " => a.wrapping_add(b),
                    _ => unreachable!(),
                };
            }
        }
        if let Some(value) = s.strip_suffix(" instanceof sample.IMActivity") {
            let object = expr(value, vars, stack, calls);
            return i32::from(object != 0 && stack[(object - 1) as usize] == Some(true));
        }
        if let Some(value) = s.strip_prefix("(java.lang.Object) ") {
            return expr(value, vars, stack, calls);
        }
        if s == "sample.Stack.count()" {
            calls.count += 1;
            return stack.len() as i32;
        }
        if let Some(arg) = s
            .strip_prefix("sample.Stack.at(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let index = expr(arg, vars, stack, calls) as usize;
            calls.indexes.push(index);
            return if stack[index].is_some() {
                index as i32 + 1
            } else {
                0
            };
        }
        match s {
            "null" | "false" => 0,
            "true" => 1,
            _ => s.parse().unwrap_or_else(|_| {
                *vars
                    .get(s)
                    .unwrap_or_else(|| panic!("unknown expression {s}"))
            }),
        }
    }
    fn end(lines: &[&str], start: usize) -> usize {
        let mut depth = 1;
        for (i, line) in lines.iter().enumerate().skip(start) {
            if line.ends_with('{') {
                depth += 1;
            }
            if *line == "}" {
                depth -= 1;
            }
            if depth == 0 {
                return i;
            }
        }
        panic!("unclosed generated block");
    }
    fn block(
        lines: &[&str],
        vars: &mut HashMap<String, i32>,
        stack: &[Option<bool>],
        calls: &mut Calls,
        fuel: &mut usize,
    ) -> Flow {
        let mut pc = 0;
        while pc < lines.len() {
            assert!(*fuel > 0);
            *fuel -= 1;
            let line = lines[pc];
            if let Some(cond) = line.strip_prefix("if ") {
                let yes_end = end(lines, pc + 1);
                let else_start = yes_end + 1;
                let has_else = lines.get(else_start) == Some(&"else {");
                let all_end = if has_else {
                    end(lines, else_start + 1)
                } else {
                    yes_end
                };
                let body = if expr(cond.trim_end_matches('{'), vars, stack, calls) != 0 {
                    &lines[pc + 1..yes_end]
                } else if has_else {
                    &lines[else_start + 1..all_end]
                } else {
                    &[]
                };
                match block(body, vars, stack, calls, fuel) {
                    Flow::Next => {}
                    f => return f,
                }
                pc = all_end + 1;
                continue;
            }
            if line == "while (true) {" {
                let finish = end(lines, pc + 1);
                loop {
                    match block(&lines[pc + 1..finish], vars, stack, calls, fuel) {
                        Flow::Break => break,
                        Flow::Return(r) => return Flow::Return(r),
                        Flow::Next => {}
                    }
                }
                pc = finish + 1;
                continue;
            }
            if line == "break;" {
                return Flow::Break;
            }
            if let Some(ret) = line.strip_prefix("return ") {
                return Flow::Return(expr(ret.trim_end_matches(';'), vars, stack, calls));
            }
            if let Some((left, right)) = line.trim_end_matches(';').split_once(" = ") {
                let value = expr(right, vars, stack, calls);
                vars.insert(left.split_whitespace().last().unwrap().into(), value);
            } else {
                assert!(
                    line.ends_with(';') && line.split_whitespace().count() == 2,
                    "unknown statement {line}"
                );
            }
            pc += 1;
        }
        Flow::Next
    }
    let separated = source.replace("} else {", "}\nelse {");
    let lines: Vec<_> = separated
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut calls = Calls::default();
    let Flow::Return(result) = block(
        &lines[1..lines.len() - 1],
        &mut HashMap::new(),
        stack,
        &mut calls,
        &mut 10000,
    ) else {
        panic!("Java failed to return")
    };
    (result, calls)
}

fn execute_dex(words: &[u16], stack: &[Option<bool>]) -> (i32, Calls) {
    let mut regs = [0_i32; 4];
    let mut pc = 0;
    let mut pending = 0;
    let mut calls = Calls::default();
    for _ in 0..10000 {
        let word = words[pc];
        let a = (word >> 8) as usize;
        match word as u8 {
            0x71 => {
                pending = if words[pc + 1] == 0 {
                    calls.count += 1;
                    stack.len() as i32
                } else {
                    let i = regs[(words[pc + 2] & 15) as usize] as usize;
                    calls.indexes.push(i);
                    if stack[i].is_some() { i as i32 + 1 } else { 0 }
                };
                pc += 3;
            }
            0x0a | 0x0c => {
                regs[a] = pending;
                pc += 1;
            }
            0x12 => {
                regs[a & 15] = (word as i16 >> 12) as i32;
                pc += 1;
            }
            0x20 => {
                let obj = regs[a >> 4];
                regs[a & 15] = i32::from(obj != 0 && stack[(obj - 1) as usize] == Some(true));
                pc += 2;
            }
            0xd8 => {
                regs[a] = regs[(words[pc + 1] & 255) as usize]
                    .wrapping_add((words[pc + 1] as i16 >> 8) as i32);
                pc += 2;
            }
            op @ (0x38 | 0x3a | 0x3d) => {
                let take = match op {
                    0x38 => regs[a] == 0,
                    0x3a => regs[a] < 0,
                    _ => regs[a] <= 0,
                };
                pc = if take {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x28 => pc = (pc as isize + (word as i16 >> 8) as isize) as usize,
            0x11 => return (regs[a], calls),
            other => panic!("unexpected DEX opcode {other:x}"),
        }
    }
    panic!("DEX failed to return")
}

#[test]
fn generated_loop_preserves_first_match_return_and_exact_call_order() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    // Exhaust all empty/nonmatching/matching/null stacks through length six.
    for length in 0..=6 {
        for mut bits in 0..3_usize.pow(length) {
            let stack: Vec<_> = (0..length)
                .map(|_| {
                    let value = match bits % 3 {
                        0 => None,
                        1 => Some(false),
                        _ => Some(true),
                    };
                    bits /= 3;
                    value
                })
                .collect();
            let (expected, expected_calls) = execute_dex(words, &stack);
            let (actual, actual_calls) = execute_java(&source, &stack);
            assert_eq!(actual, expected, "stack {stack:?}\n{source}");
            assert_eq!(actual_calls.count, 1);
            assert_eq!(actual_calls.count, expected_calls.count);
            assert_eq!(
                actual_calls.indexes, expected_calls.indexes,
                "stack {stack:?}"
            );
        }
    }
}
