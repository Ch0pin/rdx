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
        Continue,
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
                        Flow::Next | Flow::Continue => {}
                    }
                }
                pc = finish + 1;
                continue;
            }
            if line == "break;" {
                return Flow::Break;
            }
            if line == "continue;" {
                return Flow::Continue;
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

#[test]
fn common_loop_exit_break_preserves_accumulated_value() {
    for limit in 0..8 {
        for break_at in 0..8 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "sumUntil".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 4;
            code.ins = 0;
            // v0=index, v1=sum, v2=limit, v3=breakAt. Both the
            // header and body exit to the same return, but carry different sums.
            code.instructions = vec![
                0x0012, 0x0112, 0x0213, limit, 0x0313, break_at, 0x2032, 8, 0x01b0, 0x3032, 5,
                0x00d8, 0x0100, 0xf928, 0x010f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let expected: i32 = (0..limit)
                .take_while(|i| *i <= break_at)
                .map(i32::from)
                .sum();
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(
                actual, expected,
                "limit={limit}, breakAt={break_at}\n{source}"
            );
        }
    }
}

#[test]
fn loop_exit_after_conditional_body_keeps_test_order() {
    for limit in 0..8 {
        for skip in 0..8 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "sumExcept".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 4;
            code.ins = 0;
            // The first branch skips a sum update, not the loop body. Testing
            // the limit before the update would lose the final term.
            code.instructions = vec![
                0x0012, 0x0112, 0x0213, limit, 0x0313, skip, 0x3032, 3, 0x01b0, 0x2032, 5, 0x00d8,
                0x0100, 0xf928, 0x010f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let expected: i32 = (0..=limit).filter(|i| *i != skip).map(i32::from).sum();
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(actual, expected, "limit={limit}, skip={skip}\n{source}");
        }
    }
}

#[test]
fn nested_natural_loops_preserve_independent_counters() {
    for rows in 0..6 {
        for columns in 0..6 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "rectangleSize".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 5;
            code.ins = 0;
            code.instructions = vec![
                0x0012, 0x0112, 0x0313, rows, 0x0413, columns, 0x3132, 13, 0x0212, 0x4232, 7,
                0x00d8, 0x0100, 0x02d8, 0x0102, 0xfa28, 0x01d8, 0x0101, 0xf428, 0x000f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            assert_eq!(source.matches("while (true)").count(), 2, "{source}");
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(actual, i32::from(rows * columns), "{source}");
        }
    }
}

#[test]
fn conditional_latch_and_interior_break_share_correct_exit_values() {
    for limit in 1..8 {
        for break_at in 0..8 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "sumDoWhile".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 4;
            code.ins = 0;
            code.instructions = vec![
                0x0012, 0x0112, 0x0213, limit, 0x0313, break_at, 0x01b0, 0x3032, 6, 0x00d8, 0x0100,
                0x2033, 0xfffb, 0x010f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let expected: i32 = (0..limit)
                .take_while(|i| *i <= break_at)
                .map(i32::from)
                .sum();
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(
                actual, expected,
                "limit={limit}, breakAt={break_at}\n{source}"
            );
        }
    }
}

#[test]
fn nested_loop_interior_entry_is_still_rejected() {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.name = "invalidNested".into();
    method.access_flags = 9;
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 0;
    // A branch before the outer header jumps directly into the inner body,
    // bypassing both its guard and its counter initialization.
    code.instructions = vec![
        0x0012, 0x0112, 0x0313, 2, 0x0413, 2, 0x0038, 7, 0x3132, 13, 0x0212, 0x4232, 7, 0x00d8,
        0x0100, 0x02d8, 0x0102, 0xfa28, 0x01d8, 0x0101, 0xf428, 0x000f,
    ];
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn switch_after_loop_is_not_rejected_as_loop_interior() {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.name = "switchAfterLoop".into();
    method.access_flags = 9;
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 0;
    code.instructions = vec![
        0x0012, 0x2112, 0x1032, 5, 0x00d8, 0x0100, 0xfc28, 0x002b, 7, 0, 0x000f, 0x7012, 0x000f, 0,
        0x0100, 1, 2, 0, 4, 0,
    ];
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"), "{source}");
    assert!(source.contains("switch ("), "{source}");
    assert!(source.contains("case 2:"), "{source}");
    assert!(source.contains("return 7;"), "{source}");
}

#[test]
fn final_unconditional_backedge_with_internal_return_is_terminal() {
    for limit in 0..8_u16 {
        let mut class = fixture();
        let method = &mut class.methods[0];
        method.name = "untilEqual".into();
        method.access_flags = 9;
        method.return_type = "I".into();
        let code = method.code.as_mut().unwrap();
        code.registers = 2;
        code.ins = 0;
        code.instructions = vec![
            0x0012,
            0x0112 | (limit << 12),
            0x1033,
            3,
            0x000f,
            0x00d8,
            0x0100,
            0xfb28,
        ];
        let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .unwrap()
            .source;
        let (actual, _) = execute_java(&source, &[]);
        assert_eq!(actual, i32::from(limit), "{source}");
    }
}

#[test]
fn shared_nonvoid_return_after_latch_preserves_selected_register() {
    for limit in 0..8_u16 {
        for break_at in 0..8_u16 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "firstLimit".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 3;
            code.ins = 0;
            code.instructions = vec![
                0x0012,
                0x0112 | (limit << 12),
                0x0212 | (break_at << 12),
                0x1032,
                8,
                0x2032,
                5,
                0x00d8,
                0x0100,
                0xfa28,
                0x000f,
                0x010f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(actual, i32::from(limit.min(break_at)), "{source}");
        }
    }
}

#[test]
fn conditional_loop_returning_exit_tail_preserves_both_exits() {
    for limit in 0..8_u16 {
        for break_at in 1..8_u16 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "tailReturn".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 3;
            code.ins = 0;
            code.instructions = vec![
                0x0012,
                0x0112 | (limit << 12),
                0x0212 | (break_at << 12),
                0x1032,
                7,
                0x00d8,
                0x0100,
                0x2033,
                0xfffc,
                0x000f,
                0x010f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(actual, i32::from(limit.min(break_at)), "{source}");
        }
    }
}

#[test]
fn loop_exit_can_skip_sibling_branch_in_address_order() {
    for limit in 0..8_u16 {
        for enabled in 0..=1_u16 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "loopOrConstant".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 3;
            code.ins = 0;
            code.instructions = vec![
                0x0012,
                0x0112 | (limit << 12),
                0x0212 | (enabled << 12),
                0x0238,
                7,
                0x1032,
                7,
                0x00d8,
                0x0100,
                0xfc28,
                0x7012,
                0x0128,
                0x000f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let (actual, _) = execute_java(&source, &[]);
            assert_eq!(
                actual,
                if enabled == 0 { 7 } else { i32::from(limit) },
                "{source}"
            );
        }
    }
}

#[test]
fn loop_escape_computed_terminal_tail_preserves_branch_values() {
    for limit in 0..8_u16 {
        for break_at in 0..8_u16 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "computedEscape".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 3;
            code.ins = 0;
            code.instructions = vec![
                0x0012,
                0x0112 | (limit << 12),
                0x0212 | (break_at << 12),
                0x1032,
                7,
                0x2032,
                6,
                0x00d8,
                0x0100,
                0xfa28,
                0x010f,
                0x00d8,
                0x0500,
                0x1032,
                4,
                0x00d8,
                0x0100,
                0x000f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            let expected = if break_at < limit {
                let tail = break_at + 5;
                tail + u16::from(tail != limit)
            } else {
                limit
            };
            assert_eq!(
                execute_java(&source, &[]).0,
                i32::from(expected),
                "{source}"
            );
        }
    }
}

#[test]
fn backward_shared_tails_inside_outer_loop_are_not_nested_loops() {
    for limit in 0..8_u16 {
        let mut class = fixture();
        let method = &mut class.methods[0];
        method.name = "sharedTail".into();
        method.access_flags = 9;
        method.return_type = "I".into();
        let code = method.code.as_mut().unwrap();
        code.registers = 4;
        code.ins = 0;
        code.instructions = vec![
            0x0012,
            0x0112 | (limit << 12),
            0x0212,
            0x1032,
            15,
            0x0038,
            6,
            0x2312,
            0x0628,
            0x3312,
            0x0428,
            0x0038,
            0xfffe,
            0xfa28,
            0x32b0,
            0x00d8,
            0x0100,
            0xf228,
            0x020f,
        ];
        let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .unwrap()
            .source;
        assert_eq!(source.matches("while (true)").count(), 1, "{source}");
        assert_eq!(
            execute_java(&source, &[]).0,
            if limit == 0 {
                0
            } else {
                i32::from(2 * limit + 1)
            },
            "{source}"
        );
    }
}

fn contained_switch_loop(limit: u16) -> String {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.name = "switchLoop".into();
    method.access_flags = 9;
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 0;
    // Each iteration adds 3 for index zero, 1 otherwise. The switch join
    // precedes the loop latch, so its breaks must leave only the switch.
    code.instructions = vec![
        0x0012,
        0x0112 | (limit << 12),
        0x0212,
        0x1032,
        13,
        0x002b,
        13,
        0,
        0x02d8,
        0x0102,
        0x0328,
        0x02d8,
        0x0302,
        0x00d8,
        0x0100,
        0xf428,
        0x020f,
        0,
        0x0100,
        1,
        0,
        0,
        6,
        0,
    ];
    native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn switch_contained_in_loop_keeps_separate_break_scopes() {
    let source = contained_switch_loop(4);
    assert_eq!(source.matches("while (true)").count(), 1, "{source}");
    assert_eq!(source.matches("switch (").count(), 1, "{source}");
    assert!(source.contains("case 0:"), "{source}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn contained_switch_loop_jvm_matches_independent_sum() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-switch-loop-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut java = String::from("public class SwitchLoop {\n");
    for limit in 0..8 {
        java.push_str(
            &contained_switch_loop(limit).replace("switchLoop(", &format!("case{limit}(")),
        );
    }
    java.push_str("public static void main(String[] args) {\n");
    for limit in 0..8 {
        let expected = if limit == 0 { 0 } else { limit + 2 };
        java.push_str(&format!(
            "if(case{limit}() != {expected}) throw new AssertionError(\"limit {limit}\");\n"
        ));
    }
    java.push_str("}}\n");
    fs::write(dir.join("SwitchLoop.java"), &java).unwrap();
    for (program, argument) in [("javac", "SwitchLoop.java"), ("java", "SwitchLoop")] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn conditional_loop_exit_values_initialized_by_header_are_preserved() {
    for limit in 0..8_u16 {
        for stop in 1..8_u16 {
            for constant in [false, true] {
                let mut class = fixture();
                let method = &mut class.methods[0];
                method.name = "headerExit".into();
                method.access_flags = 9;
                method.return_type = "I".into();
                let code = method.code.as_mut().unwrap();
                code.registers = 4;
                code.ins = 0;
                code.instructions = vec![0x0012, 0x0112 | (limit << 12), 0x0212 | (stop << 12)];
                if constant {
                    code.instructions.extend([0x7312, 0]);
                } else {
                    code.instructions.extend([0x03d8, 0x0500]);
                }
                code.instructions
                    .extend([0x1032, 7, 0x00d8, 0x0100, 0x2033, 0xfffa, 0x000f, 0x030f]);
                let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                    .unwrap()
                    .source;
                let expected = if limit < stop {
                    if constant { 7 } else { limit + 5 }
                } else {
                    stop
                };
                assert_eq!(
                    execute_java(&source, &[]).0,
                    i32::from(expected),
                    "limit={limit},stop={stop},constant={constant}\n{source}"
                );
            }
        }
    }
}

#[test]
fn header_initialized_null_exit_stays_a_null_reference() {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.name = "nullHeaderExit".into();
    method.access_flags = 9;
    method.return_type = "Ljava/lang/Object;".into();
    method.parameters = vec!["Ljava/lang/Object;".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    // index=0, limit=2, stop=1; header defines a DEX zero used as null.
    code.instructions = vec![
        0x0012, 0x2112, 0x1212, 0x0312, 0, 0x1032, 7, 0x00d8, 0x0100, 0x2033, 0xfffa, 0x0411,
        0x0311,
    ];
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("return null;"), "{source}");
}

#[test]
fn search_loop_merges_success_and_default_exit_tails() {
    for limit in 0..8_u16 {
        for stop in 1..8_u16 {
            let mut class = fixture();
            let method = &mut class.methods[0];
            method.name = "searchExit".into();
            method.access_flags = 9;
            method.return_type = "I".into();
            let code = method.code.as_mut().unwrap();
            code.registers = 3;
            code.ins = 0;
            code.instructions = vec![
                0x0012,
                0x0112 | (limit << 12),
                0x0212 | (stop << 12),
                0x1032,
                7,
                0x00d8,
                0x0100,
                0x2033,
                0xfffc,
                0x0228,
                0xf012,
                0x000f,
            ];
            let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source;
            assert_eq!(
                execute_java(&source, &[]).0,
                if limit < stop { -1 } else { i32::from(stop) },
                "{source}"
            );
        }
    }
}

#[test]
fn interior_break_uses_values_established_by_dominating_prefix() {
    for limit in 0..8_u16 {
        let mut class = fixture();
        let method = &mut class.methods[0];
        method.name = "prefixExit".into();
        method.access_flags = 9;
        method.return_type = "I".into();
        let code = method.code.as_mut().unwrap();
        code.registers = 3;
        code.ins = 0;
        code.instructions = vec![
            0x0012,
            0x0112 | (limit << 12),
            0x02d8,
            0x0500,
            0x0038,
            3,
            0,
            0x1032,
            5,
            0x00d8,
            0x0100,
            0xf728,
            0x020f,
        ];
        let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .unwrap()
            .source;
        assert_eq!(
            execute_java(&source, &[]).0,
            i32::from(limit + 5),
            "{source}"
        );
    }
}
