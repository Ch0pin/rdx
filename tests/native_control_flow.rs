//! Behavioral branch reconstruction checks without a Java runtime or compiler.
use rdx::{
    native_dex::{self, DexClass},
    native_java,
};
use std::{collections::HashMap, sync::Arc};

fn fixture(words: &[u16], parameters: &[&str], registers: u16) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.parameters = parameters.iter().map(|p| Arc::from(*p)).collect();
    method.access_flags = 9;
    method.return_type = Arc::from("I");
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = parameters.len() as u16;
    code.instructions = words.to_vec();
    class
}
fn render(class: &DexClass) -> String {
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}

// Independent interpreter restricted to the instructions deliberately exercised
// below. Branch destinations are DEX code-unit offsets, not decoded indices.
fn dex(words: &[u16], register_count: usize, inputs: &[i32]) -> i32 {
    let mut regs = vec![0; register_count];
    regs[register_count - inputs.len()..].copy_from_slice(inputs);
    let mut pc = 0;
    for _ in 0..10000 {
        let word = words[pc];
        match word & 255 {
            0x00 => pc += 1,
            0x01 => {
                regs[((word >> 8) & 15) as usize] = regs[(word >> 12) as usize];
                pc += 1;
            }
            0x90 => {
                let arg = words[pc + 1];
                regs[(word >> 8) as usize] =
                    regs[(arg & 255) as usize].wrapping_add(regs[(arg >> 8) as usize]);
                pc += 2;
            }
            0xda => {
                let arg = words[pc + 1];
                regs[(word >> 8) as usize] =
                    regs[(arg & 255) as usize].wrapping_mul((arg as i16 >> 8) as i32);
                pc += 2;
            }
            0x12 => {
                regs[((word >> 8) & 15) as usize] = (word as i16 >> 12) as i32;
                pc += 1;
            }
            0x0f => return regs[(word >> 8) as usize],
            op @ (0x2b | 0x2c) => {
                let signed = |at: usize| (words[at] as u32 | ((words[at + 1] as u32) << 16)) as i32;
                let payload = (pc as isize + signed(pc + 1) as isize) as usize;
                let count = words[payload + 1] as usize;
                let value = regs[(word >> 8) as usize];
                let selected = if op == 0x2b {
                    let first = signed(payload + 2);
                    (0..count).find(|i| first as i64 + *i as i64 == value as i64)
                } else {
                    (0..count).find(|i| signed(payload + 2 + i * 2) == value)
                };
                pc = if let Some(index) = selected {
                    let targets = payload + if op == 0x2b { 4 } else { 2 + count * 2 };
                    (pc as isize + signed(targets + index * 2) as isize) as usize
                } else {
                    pc + 3
                };
            }
            0x28 => pc = (pc as isize + ((word as i16) >> 8) as isize) as usize,
            op @ 0x32..=0x3d => {
                let (left, right, comparison) = if op <= 0x37 {
                    (
                        regs[((word >> 8) & 15) as usize],
                        regs[(word >> 12) as usize],
                        op - 0x32,
                    )
                } else {
                    (regs[(word >> 8) as usize], 0, op - 0x38)
                };
                let take = match comparison {
                    0 => left == right,
                    1 => left != right,
                    2 => left < right,
                    3 => left >= right,
                    4 => left > right,
                    5 => left <= right,
                    _ => unreachable!(),
                };
                pc = if take {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0xd8 => {
                let arg = words[pc + 1];
                regs[(word >> 8) as usize] =
                    regs[(arg & 255) as usize].wrapping_add((arg as i16 >> 8) as i32);
                pc += 2;
            }
            op => panic!("unexpected test DEX opcode {op:x}"),
        }
    }
    panic!("test DEX did not terminate")
}

// Parse the generated Java's braces into a tiny independent AST. No production
// decoding/rendering helper participates in evaluation. References use zero for
// null and distinct nonzero identities; booleans use 0/1.
#[derive(Debug)]
enum Statement {
    If(String, Vec<Statement>, Vec<Statement>),
    While(String, Vec<Statement>),
    Switch(String, Vec<SwitchArm>),
    Break,
    Assign(String, String),
    Return(String),
}
#[derive(Debug)]
struct SwitchArm {
    keys: Vec<i32>,
    default: bool,
    body: Vec<Statement>,
}
fn statements(source: &str) -> Vec<Statement> {
    let body = &source[source.find('{').unwrap()..];
    let separated = body
        .replace('{', "{\n")
        .replace('}', "\n}\n")
        .replace(';', ";\n");
    let lines: Vec<_> = separated
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    fn block(lines: &[&str], cursor: &mut usize) -> Vec<Statement> {
        let mut result = Vec::new();
        while *cursor < lines.len() {
            let line = lines[*cursor];
            *cursor += 1;
            if line == "}" {
                break;
            }
            if let Some(selector) = line.strip_prefix("switch ") {
                let selector = selector.trim_end_matches('{').trim().to_string();
                let mut arms = Vec::new();
                while lines[*cursor] != "}" {
                    let mut keys = Vec::new();
                    let mut default = false;
                    loop {
                        let label = lines[*cursor];
                        *cursor += 1;
                        if let Some(key) = label.strip_prefix("case ") {
                            keys.push(key.split(':').next().unwrap().trim().parse().unwrap());
                        } else {
                            assert!(
                                label.starts_with("default:"),
                                "invalid switch label {label}"
                            );
                            default = true;
                        }
                        if label.ends_with('{') {
                            break;
                        }
                        if lines.get(*cursor) == Some(&"{") {
                            *cursor += 1;
                            break;
                        }
                    }
                    arms.push(SwitchArm {
                        keys,
                        default,
                        body: block(lines, cursor),
                    });
                }
                *cursor += 1;
                result.push(Statement::Switch(selector, arms));
            } else if let Some(cond) = line.strip_prefix("while ") {
                let cond = cond.trim_end_matches('{').trim().to_string();
                result.push(Statement::While(cond, block(lines, cursor)));
            } else if line == "break;" {
                result.push(Statement::Break);
            } else if let Some(cond) = line.strip_prefix("if ") {
                let cond = cond.trim_end_matches('{').trim().to_string();
                let yes = block(lines, cursor);
                let no = if lines.get(*cursor).is_some_and(|s| s.starts_with("else")) {
                    *cursor += 1;
                    block(lines, cursor)
                } else {
                    vec![]
                };
                result.push(Statement::If(cond, yes, no));
            } else if let Some(value) = line.strip_prefix("return ") {
                result.push(Statement::Return(value.trim_end_matches(';').to_string()));
            } else if let Some((name, value)) = line.trim_end_matches(';').split_once(" = ") {
                result.push(Statement::Assign(
                    name.split_whitespace().last().unwrap().to_string(),
                    value.to_string(),
                ));
            } else {
                assert!(
                    line.ends_with(';') && line.split_whitespace().count() == 2,
                    "unhandled Java statement {line}"
                );
            }
        }
        result
    }
    let mut cursor = 1;
    block(&lines, &mut cursor)
}
fn expression(source: &str, vars: &HashMap<String, i32>) -> i32 {
    let mut source = source.trim();
    while source.starts_with('(') && source.ends_with(')') {
        let mut depth = 0;
        let wraps = source.char_indices().all(|(i, c)| {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            depth != 0 || i == source.len() - 1
        });
        if !wraps {
            break;
        }
        source = &source[1..source.len() - 1];
    }
    if let Some(value) = source.strip_prefix("(java.lang.Object) ") {
        return expression(value, vars);
    }
    for op in [" == ", " != ", " <= ", " >= ", " < ", " > ", " + ", " * "] {
        if let Some((a, b)) = source.split_once(op) {
            let (a, b) = (expression(a, vars), expression(b, vars));
            return match op {
                " == " => i32::from(a == b),
                " != " => i32::from(a != b),
                " <= " => i32::from(a <= b),
                " >= " => i32::from(a >= b),
                " < " => i32::from(a < b),
                " > " => i32::from(a > b),
                " + " => a.wrapping_add(b),
                " * " => a.wrapping_mul(b),
                _ => unreachable!(),
            };
        }
    }
    if let Some(rest) = source.strip_prefix('!') {
        return i32::from(expression(rest, vars) == 0);
    }
    match source {
        "null" | "false" => 0,
        "true" => 1,
        _ => source.parse().unwrap_or_else(|_| {
            *vars
                .get(source)
                .unwrap_or_else(|| panic!("unknown Java operand {source}"))
        }),
    }
}
fn java(source: &str, inputs: &[i32]) -> i32 {
    enum Flow {
        Next,
        Break,
        Return(i32),
    }
    fn execute(code: &[Statement], vars: &mut HashMap<String, i32>, fuel: &mut usize) -> Flow {
        for statement in code {
            assert!(*fuel > 0, "generated Java loop exceeded test budget");
            *fuel -= 1;
            match statement {
                Statement::If(condition, yes, no) => {
                    let branch = if expression(condition, vars) != 0 {
                        yes
                    } else {
                        no
                    };
                    match execute(branch, vars, fuel) {
                        Flow::Next => {}
                        flow => return flow,
                    }
                }
                Statement::While(condition, body) => {
                    while expression(condition, vars) != 0 {
                        assert!(*fuel > 0, "generated Java loop exceeded test budget");
                        *fuel -= 1;
                        match execute(body, vars, fuel) {
                            Flow::Break => break,
                            Flow::Return(value) => return Flow::Return(value),
                            Flow::Next => {}
                        }
                    }
                }
                Statement::Switch(selector, arms) => {
                    let key = expression(selector, vars);
                    let arm = arms
                        .iter()
                        .find(|a| a.keys.contains(&key))
                        .or_else(|| arms.iter().find(|a| a.default))
                        .expect("switch missing default");
                    match execute(&arm.body, vars, fuel) {
                        Flow::Next | Flow::Break => {}
                        flow => return flow,
                    }
                }
                Statement::Break => return Flow::Break,
                Statement::Assign(name, value) => {
                    let value = expression(value, vars);
                    vars.insert(name.clone(), value);
                }
                Statement::Return(value) => return Flow::Return(expression(value, vars)),
            }
        }
        Flow::Next
    }
    let mut vars = inputs
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("p{i}"), *v))
        .collect();
    match execute(&statements(source), &mut vars, &mut 10000) {
        Flow::Return(value) => value,
        _ => panic!("no Java return: {source}"),
    }
}
fn check(words: &[u16], parameters: &[&str], registers: u16, cases: &[Vec<i32>]) -> String {
    let source = render(&fixture(words, parameters, registers));
    for inputs in cases {
        assert_eq!(
            java(&source, inputs),
            dex(words, registers as usize, inputs),
            "inputs={inputs:?}\n{source}"
        );
    }
    source
}

#[test]
fn every_signed_zero_comparison_preserves_early_returns() {
    let cases: Vec<_> = [i32::MIN, -1, 0, 1, i32::MAX]
        .into_iter()
        .map(|n| vec![n])
        .collect();
    for op in 0x38..=0x3d {
        check(&[op, 4, 0x1012, 0x000f, 0xf012, 0x000f], &["I"], 1, &cases);
    }
}
#[test]
fn every_signed_register_comparison_preserves_operand_order() {
    let boundary = [i32::MIN, -1, 0, 1, i32::MAX];
    let cases: Vec<_> = boundary
        .into_iter()
        .flat_map(|a| boundary.map(move |b| vec![a, b]))
        .collect();
    for op in 0x32..=0x37 {
        check(
            &[0x1000 | op, 4, 0x1012, 0x000f, 0xf012, 0x000f],
            &["I", "I"],
            2,
            &cases,
        );
    }
}
#[test]
fn boolean_zero_comparisons_emit_java_boolean_conditions() {
    for op in [0x38, 0x39] {
        let source = check(
            &[op, 4, 0x1012, 0x000f, 0xf012, 0x000f],
            &["Z"],
            1,
            &[vec![0], vec![1]],
        );
        assert!(
            !source.contains("p0 == 0") && !source.contains("p0 != 0"),
            "{source}"
        );
    }
}
#[test]
fn reference_zero_comparisons_use_null_and_preserve_paths() {
    for op in [0x38, 0x39] {
        let source = check(
            &[op, 4, 0x1012, 0x000f, 0xf012, 0x000f],
            &["Ljava/lang/Object;"],
            1,
            &[vec![0], vec![7]],
        );
        assert!(source.contains("null"), "{source}");
    }
}
#[test]
fn diamond_merges_register_values_before_a_single_shared_tail() {
    let source = check(
        &[0x0138, 4, 0x1012, 0x0228, 0x2012, 0x00d8, 0x0300, 0x000f],
        &["I"],
        2,
        &[vec![i32::MIN], vec![0], vec![i32::MAX]],
    );
    assert_eq!(
        source.matches(" + ").count(),
        1,
        "shared tail was duplicated:\n{source}"
    );
    assert_eq!(source.matches("return ").count(), 1, "{source}");
}
#[test]
fn forward_branch_bypass_uses_the_shared_continuation_as_join() {
    // if (p0 == 0) { v0 = 1; } else if (p0 != 0) { /* skip setup */ }
    // return p0 + 1;
    //
    // The second branch can fall through to the setup block or bypass it.  Its
    // fallthrough makes the setup block reachable from both outer arms, but it
    // is not a join: the bypass must remain in the nested branch and the add is
    // the single shared continuation.
    let source = check(
        &[0x0238, 4, 0x0239, 4, 0x1012, 0x0128, 0x00d8, 0x0102, 0x000f],
        &["I"],
        3,
        &[vec![i32::MIN], vec![-1], vec![0], vec![1], vec![i32::MAX]],
    );
    assert_eq!(
        source.matches(" + ").count(),
        1,
        "duplicated tail:\n{source}"
    );
    assert_eq!(
        source.matches("return ").count(),
        1,
        "duplicated return:\n{source}"
    );
}
#[test]
fn undefined_merge_register_and_invalid_branch_types_fail_closed() {
    for (words, parameters, registers) in [
        (vec![0x0138, 3, 0x1012, 0x000f], vec!["I"], 2), // v0 undefined on taken edge
        (
            vec![0x003a, 4, 0x1012, 0x000f, 0x2012, 0x000f],
            vec!["Ljava/lang/Object;"],
            1,
        ),
    ] {
        let class = fixture(&words, &parameters, registers);
        assert!(
            native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err(),
            "accepted {words:x?}"
        );
    }
    let boolean_order = fixture(&[0x003a, 4, 0x1012, 0x000f, 0x2012, 0x000f], &["Z"], 1);
    let source =
        native_java::render_method("sample.Hello", &boolean_order, &boolean_order.methods[0])
            .unwrap()
            .source;
    assert!(source.contains("? 1 : 0"), "{source}");
    assert!(
        source.contains(" < 0") || source.contains(" >= 0"),
        "{source}"
    );
}
#[test]
fn malformed_destinations_and_self_edges_fail_closed() {
    for words in [
        vec![0x0038, 1, 0x1012, 0x000f], // conditional operand, not an instruction
        vec![0x0038, 10, 0x1012, 0x000f], // beyond method
        vec![0x0038, 4, 0x1012, 0x000f], // exactly method end
        vec![0x0038, 0, 0x1012, 0x000f], // self loop
        vec![0x0038],                    // truncated branch
        vec![0x0128, 0x0028],            // goto self loop
    ] {
        let class = fixture(&words, &["I"], 1);
        assert!(
            native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err(),
            "accepted {words:x?}"
        );
    }
}

#[test]
fn reference_register_equality_preserves_identity() {
    for op in [0x32, 0x33] {
        check(
            &[0x1000 | op, 4, 0x1012, 0x000f, 0xf012, 0x000f],
            &["Ljava/lang/Object;", "Ljava/lang/Object;"],
            2,
            &[vec![0, 0], vec![0, 7], vec![7, 0], vec![7, 7], vec![7, 8]],
        );
    }
}

#[test]
fn nested_guard_returns_preserve_all_paths() {
    // Negative => -1; zero => 0; positive => 1. Distinct early returns.
    check(
        &[
            0x003a, 8, 0x0038, 4, 0x1012, 0x000f, 0x0012, 0x000f, 0xf012, 0x000f,
        ],
        &["I"],
        1,
        &[vec![i32::MIN], vec![-1], vec![0], vec![1], vec![i32::MAX]],
    );
}

#[test]
fn while_loop_preserves_zero_one_and_many_iterations() {
    let source = check(
        &[
            0x0012, 0x0112, 0x2135, 7, 0x0090, 0x0100, 0x01d8, 0x0101, 0xfa28, 0x000f,
        ],
        &["I"],
        3,
        &[vec![-1], vec![0], vec![1], vec![2], vec![7], vec![25]],
    );
    assert!(source.contains("while ("), "{source}");
}
#[test]
fn do_while_executes_body_before_testing_condition() {
    let source = check(
        &[
            0x0012, 0x00d8, 0x0100, 0x01d8, 0xff01, 0x013c, 0xfffc, 0x000f,
        ],
        &["I"],
        2,
        &[vec![0], vec![1], vec![2], vec![8]],
    );
    assert!(source.contains("while ("), "{source}");
}
#[test]
fn loop_carried_swaps_use_parallel_copy_semantics() {
    check(
        &[
            0x1012, 0x2112, 0x0212, 0x4235, 8, 0x0301, 0x1001, 0x3101, 0x02d8, 0x0102, 0xf928,
            0x00da, 0x0a00, 0x0090, 0x0100, 0x000f,
        ],
        &["I"],
        5,
        &[vec![0], vec![1], vec![2], vec![3], vec![8]],
    );
}
#[test]
fn internal_loop_branch_merges_before_next_iteration() {
    // i=0; sum=0; while(i<p0) { if(i==0) sum+=3; else sum+=1; i++; }
    check(
        &[
            0x0012, 0x0112, 0x3035, 12, 0x0039, 5, 0x01d8, 0x0301, 0x0328, 0x01d8, 0x0101, 0x00d8,
            0x0100, 0xf528, 0x010f,
        ],
        &["I"],
        4,
        &[vec![0], vec![1], vec![2], vec![9]],
    );
}

#[test]
fn return_inside_loop_preserves_guard_and_exit_paths() {
    check(
        &[
            0x0012, 0x2035, 8, 0x0339, 3, 0x000f, 0x00d8, 0x0100, 0xf928, 0x000f,
        ],
        &["I", "I"],
        4,
        &[
            vec![0, 0],
            vec![0, 1],
            vec![1, 0],
            vec![1, 1],
            vec![7, 0],
            vec![7, 1],
        ],
    );
}

#[test]
fn multiple_entry_and_undefined_exit_loops_fail_closed() {
    for (words, parameters, registers) in [
        // Entry bypasses the loop header and enters its body.
        (
            vec![0x0038, 4, 0x013d, 5, 0x01d8, 0xff01, 0xfc28, 0x000f],
            vec!["I", "I"],
            2,
        ),
        // v0 only acquires a value if the body runs at least once.
        (
            vec![0x013d, 6, 0x1012, 0x01d8, 0xff01, 0xfb28, 0x000f],
            vec!["I"],
            2,
        ),
    ] {
        let class = fixture(&words, &parameters, registers);
        assert!(
            native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err(),
            "accepted {words:x?}"
        );
    }
}

#[test]
fn unconditional_infinite_loop_preserves_nontermination_with_bounded_execution() {
    let words = [0, 0xff28];
    let source = render(&fixture(&words, &[], 0));
    assert!(source.contains("while (true)"), "{source}");
    let dex_error = std::panic::catch_unwind(|| dex(&words, 0, &[])).unwrap_err();
    assert_eq!(
        dex_error.downcast_ref::<&str>().copied(),
        Some("test DEX did not terminate")
    );
    let java_error = std::panic::catch_unwind(|| java(&source, &[])).unwrap_err();
    assert_eq!(
        java_error.downcast_ref::<&str>().copied(),
        Some("generated Java loop exceeded test budget")
    );
}

// Assemble only the tiny switch fixtures; this does not use production codecs.
// Distinct case groups each assign a constant and join one arithmetic tail.
fn switch_fixture(op: u16, keys: &[i32], groups: &[usize]) -> Vec<u16> {
    assert_eq!(keys.len(), groups.len());
    let group_count = groups.iter().copied().max().unwrap_or(0) + 1;
    let join = 5 + group_count * 2;
    let payload = join + 3;
    assert_eq!(payload % 2, 0);
    let mut words = vec![
        0x0100 | op,
        payload as u16,
        0,
        0xf012,
        (((join - 4) as u16) << 8) | 0x28,
    ];
    for group in 0..group_count {
        words.push((((group + 1) as u16) << 12) | 0x12);
        let pc = words.len();
        words.push((((join - pc) as u16) << 8) | 0x28);
    }
    words.extend([0x00d8, 0x0400, 0x000f]);
    words.extend([if op == 0x2b { 0x0100 } else { 0x0200 }, keys.len() as u16]);
    let push32 = |words: &mut Vec<u16>, value: i32| {
        words.extend([value as u16, ((value as u32) >> 16) as u16]);
    };
    if op == 0x2b {
        push32(&mut words, keys[0]);
    } else {
        for key in keys {
            push32(&mut words, *key);
        }
    }
    for group in groups {
        push32(&mut words, (5 + group * 2) as i32);
    }
    words
}

#[test]
fn packed_switch_signed_keys_and_default_preserve_shared_tail() {
    let words = switch_fixture(0x2b, &[-1, 0, 1], &[0, 1, 2]);
    let source = check(
        &words,
        &["I"],
        2,
        &[
            vec![i32::MIN],
            vec![-2],
            vec![-1],
            vec![0],
            vec![1],
            vec![2],
            vec![i32::MAX],
        ],
    );
    assert_eq!(
        source.matches(" + ").count(),
        1,
        "duplicated tail: {source}"
    );
    assert_eq!(
        source.matches("return ").count(),
        1,
        "duplicated return: {source}"
    );
}

#[test]
fn sparse_switch_extreme_signed_keys_and_default_preserve_results() {
    let words = switch_fixture(0x2c, &[i32::MIN, 7, i32::MAX], &[0, 1, 2]);
    check(
        &words,
        &["I"],
        2,
        &[
            vec![i32::MIN],
            vec![-1],
            vec![0],
            vec![7],
            vec![8],
            vec![i32::MAX],
        ],
    );
}

#[test]
fn switch_multiple_keys_sharing_target_do_not_duplicate_arm_or_tail() {
    for op in [0x2b, 0x2c] {
        let words = switch_fixture(op, &[3, 4, 5], &[0, 0, 1]);
        let source = check(
            &words,
            &["I"],
            2,
            &[vec![2], vec![3], vec![4], vec![5], vec![6]],
        );
        assert_eq!(
            source.matches(" + ").count(),
            1,
            "duplicated tail: {source}"
        );
    }
}

#[test]
fn malformed_switch_payloads_and_targets_fail_closed() {
    let good = switch_fixture(0x2c, &[-1, 0, 1], &[0, 1, 2]);
    let payload = good[1] as usize;
    let targets = payload + 2 + 6;
    let mut malformed = Vec::new();
    let mut words = good.clone();
    words[1] = 1;
    malformed.push(words); // instruction operand
    let mut words = good.clone();
    words[payload] = 0x0100;
    malformed.push(words); // mismatched tag
    let mut words = good.clone();
    words[payload + 1] = 100;
    malformed.push(words); // truncated tables
    let mut words = good.clone();
    words[payload + 4] = 0xffff;
    words[payload + 5] = 0xffff;
    malformed.push(words); // repeated sparse key
    let mut words = good.clone();
    words[targets] = 1;
    malformed.push(words); // case enters operand
    let mut words = good.clone();
    words[targets] = 0xffff;
    words[targets + 1] = 0xffff;
    malformed.push(words); // negative target
    let mut words = good.clone();
    words[targets] = payload as u16;
    malformed.push(words); // case enters payload
    let mut words = good.clone();
    words.pop();
    malformed.push(words); // truncated offset
    let mut words = switch_fixture(0x2b, &[i32::MAX, 0, 0], &[0, 1, 2]);
    words[payload + 2] = 0xffff;
    malformed.push(words); // packed keys overflow i32
    for words in malformed {
        let class = fixture(&words, &["I"], 2);
        assert!(
            native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err(),
            "accepted malformed switch {words:x?}"
        );
    }
}

#[test]
fn switch_terminal_arms_preserve_returns_without_shared_tail() {
    check(
        &[
            0x012b, 10, 0, 0x010f, 0x1012, 0x000f, 0x2012, 0x000f, 0x3012, 0x000f, 0x0100, 3,
            0xffff, 0xffff, 4, 0, 6, 0, 8, 0,
        ],
        &["I"],
        2,
        &[vec![-2], vec![-1], vec![0], vec![1], vec![2]],
    );
}

#[test]
fn switch_returning_arm_and_joining_arms_preserve_continuation() {
    let mut words = switch_fixture(0x2b, &[-1, 0, 1], &[0, 1, 2]);
    words[6] = 0x000f; // first case returns before the common arithmetic tail
    let source = check(
        &words,
        &["I"],
        2,
        &[vec![-2], vec![-1], vec![0], vec![1], vec![2]],
    );
    assert_eq!(
        source.matches(" + ").count(),
        1,
        "duplicated shared continuation: {source}"
    );
}

#[test]
fn switch_backedge_and_object_selector_fail_closed_but_boolean_projects_to_int() {
    let mut backwards = vec![0, 0];
    backwards.extend(switch_fixture(0x2b, &[-1, 0, 1], &[0, 1, 2]));
    let payload = 2 + backwards[3] as usize;
    backwards[payload + 4] = 0xfffe;
    backwards[payload + 5] = 0xffff;
    let class = fixture(&backwards, &["I"], 2);
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    let words = switch_fixture(0x2b, &[-1, 0, 1], &[0, 1, 2]);
    let boolean = fixture(&words, &["Z"], 2);
    let source = native_java::render_method("sample.Hello", &boolean, &boolean.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("? 1 : 0"), "{source}");

    let object = fixture(&words, &["Ljava/lang/Object;"], 2);
    assert!(native_java::render_method("sample.Hello", &object, &object.methods[0]).is_err());
}

#[test]
fn large_switches_preserve_every_key_and_default_with_bounded_analysis() {
    let keys: Vec<i32> = (-150..150).collect();
    let groups: Vec<usize> = (0..300).map(|i| i % 3).collect();
    for op in [0x2b, 0x2c] {
        let words = switch_fixture(op, &keys, &groups);
        let class = fixture(&words, &["I"], 2);
        let source = render(&class);
        for key in -151..=150 {
            assert_eq!(java(&source, &[key]), dex(&words, 2, &[key]), "key {key}");
        }
    }
    let oversized = switch_fixture(0x2b, &(0..1025).collect::<Vec<_>>(), &vec![0; 1025]);
    let class = fixture(&oversized, &["I"], 2);
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn hundreds_of_sequential_branches_keep_their_shared_continuations() {
    let mut words = vec![0x2012];
    for _ in 0..300 {
        words.extend([0x0138, 3, 0x3012]);
    }
    words.push(0x000f);
    let class = fixture(&words, &["I"], 2);
    let source = render(&class);
    for input in [-7, 0, 1, 128] {
        assert_eq!(java(&source, &[input]), dex(&words, 2, &[input]));
    }
}
