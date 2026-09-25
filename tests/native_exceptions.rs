//! Independent small DEX/Java evaluators for exception-visible register state.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{collections::HashMap, sync::Arc};

fn fixture(
    words: Vec<u16>,
    catches: Vec<(Option<&str>, u32)>,
    start: u32,
    end: u32,
    ret: &str,
) -> DexClass {
    DexClass {
        descriptor: "Lsample/Effects;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["first".into(), "second".into(), "touch".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("I".into(), vec![]), ("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Effects;".into(),
            name: "test".into(),
            return_type: ret.into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 0,
                outs: 0,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start,
                    end,
                    catches: catches
                        .into_iter()
                        .map(|(ty, addr)| (ty.map(Arc::from), addr))
                        .collect::<Vec<_>>()
                        .into(),
                }],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}

#[test]
fn discarded_wide_result_before_try_keeps_call_outside_handler() {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000b, 0x0012, 0x0071, 2, 0, 0x000e, 0x020d, 0x000e,
        ],
        vec![(Some("Ljava/lang/Throwable;"), 9)],
        5,
        9,
        "V",
    );
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].0 = "J".into();
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    let call = code.source.find("sample.Effects.first()").unwrap();
    let protected = code.source.find("try {").unwrap();
    assert!(call < protected, "{}", code.source);
    assert_eq!(code.source.matches("sample.Effects.first()").count(), 1);
    assert!(code.source[protected..].contains("sample.Effects.touch()"));
    assert!(code.source.contains("catch (java.lang.Throwable "));

    // The handler does not observe the discarded wide value: no snapshot.
    class.methods[0].code.as_mut().unwrap().try_regions[0].start = 4;
    native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
}

#[test]
fn void_return_after_try_preserves_terminal_path_but_calls_stay_outside() {
    let class = fixture(
        vec![0x0071, 2, 0, 0x000e, 0x000d, 0x000e],
        vec![(Some("Ljava/lang/Throwable;"), 4)],
        0,
        3,
        "V",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source.matches("return;").count(), 2, "{}", code.source);
    assert!(code.source.find("return;").unwrap() > code.source.find("catch (").unwrap());

    let effects = fixture(
        vec![0x0071, 2, 0, 0x0071, 2, 0, 0x000e, 0x000d, 0x000e],
        vec![(Some("Ljava/lang/Throwable;"), 7)],
        0,
        3,
        "V",
    );
    let code = native_java::render_method("sample.Effects", &effects, &effects.methods[0]).unwrap();
    assert_eq!(code.source.matches("sample.Effects.touch()").count(), 2);
    assert!(
        code.source.rfind("sample.Effects.touch()").unwrap() > code.source.find("catch (").unwrap()
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    value: Result<Option<i32>, String>,
    calls: Vec<String>,
}

fn matches_catch(catch: &str, error: &str) -> bool {
    catch == "java.lang.Throwable"
        || catch == error
        || (catch == "java.lang.Exception" && error == "java.lang.IllegalArgumentException")
}
fn call(name: &str, fail: Option<(&str, &str)>, calls: &mut Vec<String>) -> Result<i32, String> {
    calls.push(name.into());
    if let Some((method, error)) = fail
        && method == name
    {
        return Err(error.into());
    }
    Ok(match name {
        "first" => 7,
        "second" => 11,
        "touch" | "<init>" => 0,
        _ => panic!("unknown call {name}"),
    })
}

fn dex_eval(class: &DexClass, fail: Option<(&str, &str)>) -> Outcome {
    let code = class.methods[0].code.as_ref().unwrap();
    let mut regs = vec![None; code.registers as usize];
    let mut pc = 0usize;
    let mut pending = None;
    let mut calls = vec![];
    for _ in 0..100 {
        let word = code.instructions[pc];
        let op = word as u8;
        let a = (word >> 8) as usize;
        match op {
            0x01 | 0x07 => {
                regs[a & 15] = regs[a >> 4];
                pc += 1;
            }
            0x12 => {
                regs[a & 15] = Some((word as i16 >> 12) as i32);
                pc += 1;
            }
            0x1c => {
                let ty = &class.symbols.types[code.instructions[pc + 1] as usize];
                let ty = ty
                    .trim_start_matches('L')
                    .trim_end_matches(';')
                    .replace('/', ".");
                calls.push(format!("class:{ty}"));
                regs[a] = Some(match ty.as_str() {
                    "sample.First" => 2,
                    "sample.Second" => 3,
                    _ => panic!("unknown test class"),
                });
                pc += 2;
            }
            0x22 => {
                calls.push("new:sample.Box".into());
                if let Some(("new", error)) = fail {
                    return Outcome {
                        value: Err(error.into()),
                        calls,
                    };
                }
                regs[a] = Some(99);
                pc += 2;
            }
            0x70 | 0x71 => {
                let method = class.symbols.methods[code.instructions[pc + 1] as usize];
                let name = &class.symbols.strings[method.2 as usize];
                if name == "<init>" {
                    let packed = code.instructions[pc + 2];
                    let arguments: Vec<_> = (1..(a >> 4))
                        .map(|index| {
                            let register = ((packed >> (index * 4)) & 15) as usize;
                            regs[register].expect("constructor argument")
                        })
                        .collect();
                    calls.push(format!("args:{arguments:?}"));
                }
                match call(name, fail, &mut calls) {
                    Ok(value) => {
                        pending = Some(value);
                        pc += 3;
                    }
                    Err(error) => {
                        let target = code
                            .try_regions
                            .iter()
                            .find(|r| r.start as usize <= pc && pc < r.end as usize)
                            .and_then(|r| {
                                r.catches.iter().find(|(ty, _)| {
                                    ty.as_ref().is_none_or(|ty| {
                                        matches_catch(
                                            &ty.trim_start_matches('L')
                                                .trim_end_matches(';')
                                                .replace('/', "."),
                                            &error,
                                        )
                                    })
                                })
                            })
                            .map(|(_, target)| *target as usize);
                        if let Some(target) = target {
                            pc = target;
                            pending = None;
                        } else {
                            return Outcome {
                                value: Err(error),
                                calls,
                            };
                        }
                    }
                }
            }
            0x0a => {
                regs[a] = Some(pending.take().expect("DEX pending result"));
                pc += 1;
            }
            0x0d => {
                regs[a] = Some(-99);
                pc += 1;
            }
            0x28 => {
                pc = (pc as isize + (a as u8 as i8) as isize) as usize;
            }
            0x0e => {
                return Outcome {
                    value: Ok(None),
                    calls,
                };
            }
            0x0f | 0x11 => {
                return Outcome {
                    value: Ok(Some(regs[a].expect("DEX defined register"))),
                    calls,
                };
            }
            _ => panic!("unsupported test DEX opcode {op:x}"),
        }
    }
    panic!("DEX fixture exceeded step limit")
}

#[derive(Debug)]
enum Statement {
    Assign(String, Option<String>),
    Call(String),
    Return(Option<String>),
    Try(Vec<Statement>, Vec<(String, String, Vec<Statement>)>),
}
fn block(lines: &[&str], cursor: &mut usize) -> Vec<Statement> {
    let mut result = vec![];
    while *cursor < lines.len() && !lines[*cursor].starts_with('}') {
        let line = lines[*cursor];
        *cursor += 1;
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if line == "try {" {
            let body = block(lines, cursor);
            let mut catches = vec![];
            while *cursor < lines.len() && lines[*cursor].contains("catch (") {
                let catch = lines[*cursor]
                    .split("catch (")
                    .nth(1)
                    .unwrap()
                    .split(')')
                    .next()
                    .unwrap();
                let mut parts = catch.split_whitespace();
                let ty = parts.next().unwrap().to_owned();
                let name = parts.next().unwrap().to_owned();
                assert!(
                    parts.next().is_none(),
                    "multi-catch outside evaluator subset"
                );
                *cursor += 1;
                catches.push((ty, name, block(lines, cursor)));
            }
            assert_eq!(lines[*cursor], "}");
            *cursor += 1;
            result.push(Statement::Try(body, catches));
        } else if let Some(expression) = line.strip_prefix("return") {
            let expression = expression.trim_end_matches(';').trim();
            result.push(Statement::Return(
                (!expression.is_empty()).then(|| expression.into()),
            ));
        } else {
            let line = line.strip_suffix(';').expect("statement semicolon");
            if let Some((lhs, rhs)) = line.split_once(" = ") {
                result.push(Statement::Assign(
                    lhs.split_whitespace().last().unwrap().into(),
                    Some(rhs.into()),
                ));
            } else if line.ends_with("()") {
                result.push(Statement::Call(line.into()));
            } else {
                let (ty, name) = line.split_once(' ').expect("local declaration");
                assert!(
                    matches!(
                        ty,
                        "int" | "java.lang.Exception" | "java.lang.Throwable" | "java.lang.Class"
                    ),
                    "unexpected type {ty}"
                );
                result.push(Statement::Assign(name.into(), None));
            }
        }
    }
    result
}

fn java_eval(source: &str, fail: Option<(&str, &str)>) -> Outcome {
    let lines: Vec<_> = source.lines().skip(1).map(str::trim).collect();
    let statements = block(&lines, &mut 0);
    let mut locals = HashMap::new();
    let mut calls = vec![];
    fn expression(
        text: &str,
        locals: &mut HashMap<String, Option<i32>>,
        fail: Option<(&str, &str)>,
        calls: &mut Vec<String>,
    ) -> Result<i32, String> {
        if let Some(inner) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            let (name, value) = inner.split_once(" = ").expect("nested capture assignment");
            let value = expression(value, locals, fail, calls)?;
            locals.insert(name.into(), Some(value));
            return Ok(value);
        }
        if let Some(arguments) = text
            .strip_prefix("new sample.Box(")
            .and_then(|s| s.strip_suffix(')'))
        {
            calls.push("new:sample.Box".into());
            if let Some(("new", error)) = fail {
                return Err(error.into());
            }
            let mut argument_values = Vec::new();
            let mut depth = 0;
            let mut start = 0;
            for (offset, ch) in arguments.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        argument_values.push(expression(
                            arguments[start..offset].trim(),
                            locals,
                            fail,
                            calls,
                        )?);
                        start = offset + 1;
                    }
                    _ => {}
                }
            }
            if !arguments.is_empty() {
                argument_values.push(expression(arguments[start..].trim(), locals, fail, calls)?);
            }
            calls.push(format!("args:{argument_values:?}"));
            call("<init>", fail, calls)?;
            return Ok(99);
        }
        if let Some(ty) = text.strip_suffix(".class") {
            calls.push(format!("class:{ty}"));
            return Ok(match ty {
                "sample.First" => 2,
                "sample.Second" => 3,
                _ => panic!("unknown test class"),
            });
        }
        if let Some(call_name) = text.strip_suffix("()") {
            return call(call_name.rsplit('.').next().unwrap(), fail, calls);
        }
        Ok(text.parse().unwrap_or_else(|_| {
            locals
                .get(text)
                .unwrap_or_else(|| panic!("unknown Java local {text}"))
                .expect("defined Java local")
        }))
    }
    fn execute(
        statements: &[Statement],
        locals: &mut HashMap<String, Option<i32>>,
        fail: Option<(&str, &str)>,
        calls: &mut Vec<String>,
    ) -> Result<Option<Option<i32>>, String> {
        for statement in statements {
            match statement {
                Statement::Assign(name, value) => {
                    let value = value
                        .as_ref()
                        .map(|v| expression(v, locals, fail, calls))
                        .transpose()?;
                    locals.insert(name.clone(), value);
                }
                Statement::Call(expr) => {
                    expression(expr, locals, fail, calls)?;
                }
                Statement::Return(value) => {
                    return Ok(Some(
                        value
                            .as_ref()
                            .map(|v| expression(v, locals, fail, calls))
                            .transpose()?,
                    ));
                }
                Statement::Try(body, catches) => {
                    let result = match execute(body, locals, fail, calls) {
                        Err(error) => {
                            let Some((_, name, handler)) =
                                catches.iter().find(|(ty, _, _)| matches_catch(ty, &error))
                            else {
                                return Err(error);
                            };
                            locals.insert(name.clone(), Some(-99));
                            execute(handler, locals, fail, calls)?
                        }
                        Ok(result) => result,
                    };
                    if result.is_some() {
                        return Ok(result);
                    }
                }
            }
        }
        Ok(None)
    }
    let value = execute(&statements, &mut locals, fail, &mut calls)
        .map(|value| value.expect("method returned"));
    Outcome { value, calls }
}

#[test]
fn catch_observes_completed_writes_and_retains_old_value_when_call_throws() {
    let class = fixture(
        vec![
            0x2012, 0x0071, 0, 0, 0x000a, 0x0071, 1, 0, 0x000a, 0x0328, 0x010d, 0x0128, 0x000f,
        ],
        vec![(Some("Ljava/lang/Exception;"), 10)],
        1,
        9,
        "I",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for (fail, expected) in [
        (None, 11),
        (Some(("first", "java.lang.Exception")), 2),
        (Some(("second", "java.lang.Exception")), 7),
    ] {
        let dex = dex_eval(&class, fail);
        assert_eq!(dex.value, Ok(Some(expected)));
        assert_eq!(java_eval(&code.source, fail), dex, "{}", code.source);
    }
}

#[test]
fn unused_exception_handler_can_share_the_normal_void_return() {
    let class = fixture(
        vec![0x0071, 2, 0, 0x000e],
        vec![(Some("Ljava/lang/Exception;"), 3)],
        0,
        3,
        "V",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("catch (java.lang.Exception"));
    for fail in [None, Some(("touch", "java.lang.Exception"))] {
        assert_eq!(
            java_eval(&code.source, fail),
            dex_eval(&class, fail),
            "{}",
            code.source
        );
    }
}

#[test]
fn catch_order_and_uncaught_exceptions_preserve_behavior() {
    let class = fixture(
        vec![
            0x0012, 0x0071, 0, 0, 0x000a, 0x0728, 0x010d, 0x1012, 0x0428, 0x010d, 0x2012, 0x0128,
            0x000f,
        ],
        vec![
            (Some("Ljava/lang/IllegalArgumentException;"), 6),
            (Some("Ljava/lang/Exception;"), 9),
        ],
        1,
        5,
        "I",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .find("catch (java.lang.IllegalArgumentException")
            .unwrap()
            < code.source.find("catch (java.lang.Exception").unwrap()
    );
    for fail in [
        None,
        Some(("first", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.Exception")),
        Some(("first", "java.lang.Error")),
    ] {
        assert_eq!(
            java_eval(&code.source, fail),
            dex_eval(&class, fail),
            "{}",
            code.source
        );
    }
}

#[test]
fn exception_regions_and_targets_inside_instructions_are_rejected() {
    for (start, end, target) in [(1, 3, 3), (0, 2, 3), (0, 3, 1)] {
        let class = fixture(
            vec![0x0071, 2, 0, 0x000e],
            vec![(Some("Ljava/lang/Exception;"), target)],
            start,
            end,
            "V",
        );
        assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
    }
}

fn allocation(two: bool, reordered: bool) -> DexClass {
    let mut class = fixture(vec![], vec![], 0, 0, "Lsample/Box;");
    let parameters = vec![Arc::<str>::from("Ljava/lang/Class;"); if two { 2 } else { 1 }];
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into()],
        types: vec![
            "Lsample/Box;".into(),
            "Ljava/lang/Class;".into(),
            "Lsample/First;".into(),
            "Lsample/Second;".into(),
        ],
        protos: vec![("V".into(), parameters)],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![0x0022, 0, 0x011c, 2];
    if two {
        code.instructions.extend([
            0x021c,
            3,
            0x3070,
            0,
            if reordered { 0x0120 } else { 0x0210 },
        ]);
    } else {
        code.instructions.extend([0x2070, 0, 0x0010]);
    }
    code.instructions.push(0x0011);
    class
}

#[test]
fn allocation_precedes_class_resolution_and_constructor_preserves_resolution_order() {
    for two in [false, true] {
        let class = allocation(two, false);
        let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        let line = code
            .source
            .lines()
            .find(|line| line.contains("new sample.Box("))
            .unwrap();
        assert!(line.find("new sample.Box(").unwrap() < line.find("sample.First.class").unwrap());
        assert_eq!(code.source.matches("sample.First.class").count(), 1);
        if two {
            assert!(
                line.find("sample.First.class").unwrap()
                    < line.find("sample.Second.class").unwrap()
            );
        }
        for label in ["sample.First", "sample.Second"]
            .into_iter()
            .take(if two { 2 } else { 1 })
        {
            let link = code
                .links
                .iter()
                .find(|link| {
                    link.label == label
                        && code
                            .source
                            .chars()
                            .skip(link.start)
                            .take(link.end - link.start)
                            .collect::<String>()
                            == label
                })
                .unwrap();
            assert_eq!(
                code.source
                    .chars()
                    .skip(link.end)
                    .take(6)
                    .collect::<String>(),
                ".class"
            );
        }
    }
    let class = allocation(true, true);
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    // Readable staging preserves resolution order while moving allocation after it.
    let first = rendered.source.find("sample.First.class").unwrap();
    let second = rendered.source.find("sample.Second.class").unwrap();
    let constructor = rendered.source.find("new sample.Box(v1, v0)").unwrap();
    assert!(
        first < second && second < constructor,
        "{}",
        rendered.source
    );
    assert_eq!(rendered.source.matches("sample.First.class").count(), 1);
    assert_eq!(rendered.source.matches("sample.Second.class").count(), 1);
}

#[test]
fn constructor_throw_observes_resolved_class_capture() {
    let mut class = allocation(false, false);
    class.methods[0].return_type = "Ljava/lang/Class;".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 2,
        end: 9,
        catches: vec![(Some(Arc::from("Ljava/lang/Exception;")), 9)].into(),
    }];
    code.instructions = vec![0x011c, 3, 0x0022, 0, 0x011c, 2, 0x2070, 0, 0x0010, 0x0111];
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for fail in [None, Some(("<init>", "java.lang.Exception"))] {
        assert_eq!(
            java_eval(&rendered.source, fail),
            dex_eval(&class, fail),
            "{}",
            rendered.source
        );
    }
}

#[test]
fn review_superclass_before_subclass_catches_are_not_emitted_as_java_clauses() {
    let class = fixture(
        vec![
            0x0012, 0x0071, 0, 0, 0x000a, 0x0728, 0x010d, 0x1012, 0x0428, 0x010d, 0x2012, 0x0128,
            0x000f,
        ],
        vec![
            (Some("Ljava/lang/Exception;"), 6),
            (Some("Ljava/lang/IllegalArgumentException;"), 9),
        ],
        1,
        5,
        "I",
    );
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]);
    assert!(
        rendered.is_err(),
        "unreachable catch incorrectly accepted: {}",
        rendered.unwrap().source
    );
}

#[test]
fn catch_requires_established_throwable_type() {
    for ty in [
        "Ljava/lang/String;",
        "Ljava/lang/Object;",
        "Lunknown/Failure;",
    ] {
        let class = fixture(
            vec![0x0071, 2, 0, 0x000e, 0x010d, 0x000e],
            vec![(Some(ty), 4)],
            0,
            3,
            "V",
        );
        assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn review_reused_class_capture_slot_preserves_old_alias() {
    let mut class = allocation(true, false);
    class.methods[0].return_type = "Ljava/lang/Class;".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 2,
        end: 13,
        catches: vec![(Some(Arc::from("Ljava/lang/Exception;")), 13)].into(),
    }];
    code.instructions = vec![
        0x011c, 3, 0x0022, 0, 0x011c, 2, 0x1207, 0x011c, 3, 0x3070, 0, 0x0120, 0x0211, 0x0111,
    ];
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for fail in [None, Some(("<init>", "java.lang.Exception"))] {
        assert_eq!(
            java_eval(&rendered.source, fail),
            dex_eval(&class, fail),
            "{}",
            rendered.source
        );
    }
}

#[test]
fn review_exception_snapshot_preserves_move_alias_values() {
    let class = fixture(
        vec![0x2012, 0x0101, 0x7012, 0x0071, 2, 0, 0x010f, 0x000f],
        vec![(Some("Ljava/lang/Exception;"), 7)],
        1,
        7,
        "I",
    );
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for fail in [None, Some(("touch", "java.lang.Exception"))] {
        assert_eq!(
            java_eval(&rendered.source, fail),
            dex_eval(&class, fail),
            "{}",
            rendered.source
        );
    }
}

#[test]
fn deferred_capture_moved_to_observed_handler_register_is_rejected() {
    let mut class = allocation(false, false);
    class.methods[0].return_type = "Ljava/lang/Class;".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 2,
        end: 11,
        catches: vec![(Some(Arc::from("Ljava/lang/Exception;")), 11)].into(),
    }];
    code.instructions = vec![
        0x021c, 3, 0x0022, 0, 0x011c, 2, 0x1207, 0x2070, 0, 0x0010, 0x0211, 0x0211,
    ];
    let error =
        native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deferred class alias writes exception-visible"),
        "{error}"
    );
}

#[test]
fn erased_checked_catch_uses_guarded_dispatch_without_inventing_calls() {
    let class = fixture(
        vec![0x0012, 0x000f, 0x1012, 0x000f],
        vec![(Some("Ljava/io/IOException;"), 2)],
        0,
        2,
        "I",
    );
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("instanceof java.io.IOException"),
        "{}",
        rendered.source
    );
    assert!(rendered.source.contains("catch (java.lang.Throwable"));
    assert!(rendered.source.contains("throw caught"));
}

#[test]
fn unchanged_boolean_constant_survives_try_handler_analysis() {
    for value in [0u16, 1] {
        // v0 is unchanged by protected invoke/result. Both normal and catch
        // paths return that same DEX boolean constant.
        let class = fixture(
            vec![
                (value << 12) | 0x12,
                0x0071,
                0,
                0,
                0x010a,
                0x0228,
                0x010d,
                0x000f,
            ],
            vec![(Some("Ljava/lang/Exception;"), 6)],
            1,
            5,
            "Z",
        );
        let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        assert!(code.source.contains("try {"), "{}", code.source);
        assert!(
            code.source.contains("catch (java.lang.Exception"),
            "{}",
            code.source
        );
        assert!(
            code.source.contains(if value == 0 {
                "return false;"
            } else {
                "return true;"
            }),
            "{}",
            code.source
        );
    }
}

#[test]
fn wide_normal_continuation_remains_outside_terminating_catch() {
    let mut class = fixture(
        vec![0x0071, 2, 0, 0x0071, 0, 0, 0x000b, 0x0010, 0x020d, 0x0227],
        vec![(Some("Ljava/lang/RuntimeException;"), 8)],
        0,
        3,
        "J",
    );
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].0 = "J".into();
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    let call = code.source.find("sample.Effects.first()").unwrap();
    assert!(
        call > code.source.find("catch (").unwrap(),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("sample.Effects.first()").count(), 1);
    // A terminating rethrow handler does not observe the wide result.
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 7;
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.find("sample.Effects.first()").unwrap() < code.source.find("catch (").unwrap()
    );
}

#[test]
fn unchanged_caught_throwable_can_be_rethrown_after_cleanup() {
    let mut class = fixture(
        vec![0x0071, 2, 0, 0x000e, 0x000d, 0x0071, 2, 0, 0x0027],
        vec![(None, 4)],
        0,
        3,
        "V",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("catch (java.lang.Throwable"));
    assert_eq!(code.source.matches("sample.Effects.touch()").count(), 2);
    assert!(code.source.contains("throw e"));
    // Replacing the caught value with an arbitrary checked result is not precise rethrow.
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].0 = "Ljava/lang/Throwable;".into();
    class.methods[0].code.as_mut().unwrap().instructions =
        vec![0x0071, 2, 0, 0x000e, 0x000d, 0x0071, 0, 0, 0x000c, 0x0027];
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains("throws java.lang.Throwable"),
        "{}",
        code.source
    );
    assert!(
        !code.source.contains("throw e"),
        "replacement is not precise rethrow: {}",
        code.source
    );
}

#[test]
fn focused_normal_continuation_exception_behavior_and_negative_control() {
    let class = fixture(
        vec![
            0x0071, 0, 0, 0x0071, 1, 0, 0x000a, 0x000f, 0x020d, 0xf012, 0x000f,
        ],
        vec![(Some("Ljava/lang/IllegalArgumentException;"), 8)],
        0,
        3,
        "I",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for fail in [
        None,
        Some(("first", "java.lang.IllegalArgumentException")),
        Some(("second", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.Error")),
        Some(("second", "java.lang.Error")),
    ] {
        assert_eq!(
            java_eval(&code.source, fail),
            dex_eval(&class, fail),
            "{fail:?}: {}",
            code.source
        );
    }
    let mutant = "int test() {\ntry {\nsample.Effects.first();\nint x = sample.Effects.second();\nreturn x;\n} catch (java.lang.IllegalArgumentException e) {\nreturn -1;\n}\n}\n";
    let fail = Some(("second", "java.lang.IllegalArgumentException"));
    assert_ne!(
        java_eval(mutant, fail),
        dex_eval(&class, fail),
        "negative control must detect broadened catch"
    );
}

#[test]
fn focused_constructor_behavior_and_known_staging_exception_difference() {
    for two in [false, true] {
        let class = allocation(two, false);
        let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        for fail in [
            None,
            Some(("<init>", "java.lang.IllegalArgumentException")),
            Some(("new", "java.lang.OutOfMemoryError")),
        ] {
            assert_eq!(
                java_eval(&code.source, fail),
                dex_eval(&class, fail),
                "{fail:?}: {}",
                code.source
            );
        }
    }
    // This is the explicitly accepted readable-staging tradeoff, not exact equivalence.
    let class = allocation(true, true);
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for failure in [None, Some(("<init>", "java.lang.IllegalArgumentException"))] {
        let original = dex_eval(&class, failure);
        let rendered = java_eval(&code.source, failure);
        assert_eq!(original.value, rendered.value);
        let without_allocation = |trace: Vec<String>| {
            trace
                .into_iter()
                .filter(|event| event != "new:sample.Box")
                .collect::<Vec<_>>()
        };
        assert_eq!(
            without_allocation(original.calls),
            without_allocation(rendered.calls)
        );
    }
    let fail = Some(("new", "java.lang.OutOfMemoryError"));
    let dex = dex_eval(&class, fail);
    let java = java_eval(&code.source, fail);
    assert_eq!(dex.value, java.value);
    assert_ne!(dex.calls, java.calls);
    assert_eq!(dex.calls, ["new:sample.Box"]);
    assert_eq!(
        java.calls,
        [
            "class:sample.First",
            "class:sample.Second",
            "new:sample.Box"
        ]
    );
}

#[test]
fn return_only_try_exits_are_allowed_but_effectful_exits_are_not_widened() {
    // first(); result v0; if-eqz v0, null/zero tail; otherwise return v0.
    let class = fixture(
        vec![0x0071, 0, 0, 0x000a, 0x0038, 3, 0x000f, 0x0012, 0x000f],
        vec![(Some("Ljava/lang/RuntimeException;"), 7)],
        0,
        6,
        "I",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("try {"));
    assert!(code.source.contains("catch (java.lang.RuntimeException"));
    let mut effects = class;
    effects.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0038, 6, 0x0071, 2, 0, 0x000f, 0x0012, 0x000f,
    ];
    effects.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 10)].into();
    let error =
        native_java::render_method("sample.Effects", &effects, &effects.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("protected return tail crosses effectful continuation"),
        "{error:#}"
    );
}

#[test]
fn staged_constructor_inside_try_preserves_handler_and_argument_effects() {
    let mut class = allocation(true, true);
    class.methods[0].return_type = "I".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0022, 0, 0x011c, 2, 0x021c, 3, 0x3070, 0, 0x0120, 0x7012, 0x000f, 0x000d, 0xf012, 0x000f,
    ];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 9,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 11)].into(),
    }];
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    for fail in [None, Some(("<init>", "java.lang.Exception"))] {
        let mut expected = dex_eval(&class, fail);
        let mut actual = java_eval(&rendered.source, fail);
        // Existing readable-staging policy relocates allocation. All remaining
        // effects, constructor arguments and catch outcomes must still match.
        expected.calls.retain(|call| !call.starts_with("new:"));
        actual.calls.retain(|call| !call.starts_with("new:"));
        assert_eq!(actual, expected, "{}", rendered.source);
    }
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 4;
    assert!(
        native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err(),
        "staging crossed exception boundary"
    );
}

#[test]
fn absorbed_return_tail_does_not_move_unproven_cast_under_catch() {
    let mut class = fixture(
        vec![0x0071, 0, 0, 0x000c, 0x0038, 3, 0x0011, 0x0012, 0x0011],
        vec![(Some("Ljava/lang/RuntimeException;"), 7)],
        0,
        6,
        "Lsample/Box;",
    );
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].0 = "Ljava/lang/Object;".into();
    let error =
        native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("potentially throwing conversion"),
        "{error:#}"
    );
}

#[test]
fn guarded_try_can_share_its_ignored_handler_with_outer_return() {
    let class = fixture(
        vec![
            0x0071, 0, 0, 0x010a, 0x0138, 7, 0x0071, 1, 0, 0x010a, 0x010f, 0x0012, 0x000f,
        ],
        vec![(Some("Ljava/lang/Exception;"), 11)],
        6,
        10,
        "I",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("catch (java.lang.Exception"));
    assert_eq!(code.source.matches("sample.Effects.second()").count(), 1);
    assert!(code.source.contains("return 0;"), "{}", code.source);
}

#[test]
fn nonthrowing_boolean_tail_can_share_two_ignored_catches() {
    let class = fixture(
        vec![
            0x0012, 0x0071, 0, 0, 0x010a, 0x0138, 4, 0x1112, 0x010f, 0x000f,
        ],
        vec![
            (Some("Ljava/lang/UnsupportedOperationException;"), 9),
            (Some("Ljava/lang/NullPointerException;"), 9),
        ],
        1,
        5,
        "Z",
    );
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("return true;"), "{}", code.source);
    assert!(code.source.contains("return false;"), "{}", code.source);
    assert_eq!(code.source.matches("catch (").count(), 2);
    assert_eq!(code.source.matches("sample.Effects.first()").count(), 1);
}

#[test]
fn separate_typed_tries_keep_calls_in_their_own_handlers() {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x0328, 0x000d, 0x000e, 0x0071, 1, 0, 0x000e, 0x000d, 0x000e,
        ],
        vec![(Some("Ljava/lang/IllegalArgumentException;"), 4)],
        0,
        3,
        "V",
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 6,
        end: 9,
        catches: vec![(Some(Arc::from("Ljava/lang/IllegalStateException;")), 10)].into(),
    });
    let result = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    let source = &result.source;
    assert_eq!(source.matches("try {").count(), 2, "{source}");
    assert_eq!(source.matches("sample.Effects.first()").count(), 1);
    assert_eq!(source.matches("sample.Effects.second()").count(), 1);
    let first_catch = source
        .find("catch (java.lang.IllegalArgumentException")
        .unwrap();
    let second_call = source.find("sample.Effects.second()").unwrap();
    let second_catch = source
        .find("catch (java.lang.IllegalStateException")
        .unwrap();
    assert!(source.find("sample.Effects.first()").unwrap() < first_catch);
    assert!(first_catch < second_call && second_call < second_catch);
    assert!(source[first_catch..second_call].contains("try {"));

    for fail in [
        None,
        Some(("first", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.IllegalStateException")),
        Some(("second", "java.lang.IllegalStateException")),
        Some(("second", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.Error")),
        Some(("second", "java.lang.Error")),
    ] {
        assert_eq!(
            java_eval(source, fail),
            dex_eval(&class, fail),
            "{fail:?}: {source}"
        );
    }

    // Overlapping protection cannot be treated as separate sequential catches.
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 9;
    assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
}

#[test]
fn separate_typed_tries_allow_handlers_after_all_normal_code() {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x0071, 1, 0, 0x000e, 0x000d, 0x000e, 0x000d, 0x000e,
        ],
        vec![(Some("Ljava/lang/IllegalArgumentException;"), 9)],
        0,
        3,
        "V",
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 3,
        end: 6,
        catches: vec![(Some(Arc::from("Ljava/lang/IllegalStateException;")), 7)].into(),
    });
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(source.matches("try {").count(), 2, "{source}");
    for fail in [
        None,
        Some(("first", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.IllegalStateException")),
        Some(("second", "java.lang.IllegalArgumentException")),
        Some(("second", "java.lang.IllegalStateException")),
        Some(("first", "java.lang.Error")),
        Some(("second", "java.lang.Error")),
    ] {
        assert_eq!(
            java_eval(&source, fail),
            dex_eval(&class, fail),
            "{fail:?}: {source}"
        );
    }
}

#[test]
fn protected_return_tail_can_branch_without_adding_caught_effects() {
    // The protected conditional exits at 6 or 10. A pure conditional at 6
    // and a move at 9 converge on the same return, as in parseRequestId.
    let class = fixture(
        vec![
            0x0012, 0x0071, 0, 0, 0x0038, 6, 0x0038, 4, 0x0128, 0x0001, 0x000f, 0x010d, 0x000f,
        ],
        vec![(Some("Ljava/lang/RuntimeException;"), 11)],
        1,
        6,
        "I",
    );
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
    assert_eq!(source.matches("sample.Effects.first()").count(), 1);
}

#[test]
fn shared_void_return_handler_keeps_normal_effects_outside_catch() {
    let class = fixture(
        vec![0x0071, 0, 0, 0x0071, 1, 0, 0x000e],
        vec![(Some("Ljava/lang/IllegalArgumentException;"), 6)],
        0,
        3,
        "V",
    );
    let result = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        result.source.find("catch (").unwrap()
            < result.source.find("sample.Effects.second()").unwrap()
    );
    for fail in [
        None,
        Some(("first", "java.lang.IllegalArgumentException")),
        Some(("second", "java.lang.IllegalArgumentException")),
        Some(("first", "java.lang.Error")),
        Some(("second", "java.lang.Error")),
    ] {
        assert_eq!(
            java_eval(&result.source, fail),
            dex_eval(&class, fail),
            "{fail:?}: {}",
            result.source
        );
    }
}

#[test]
fn wide_try_values_without_mutable_handler_snapshots() {
    for ty in ["J", "D"] {
        // v0/v1 is established before try; the call cannot alter it. Both
        // normal and catch return the same value, including the upper word.
        let mut class = fixture(
            vec![
                0x0018, 0x1234, 0x5678, 0x9abc, 0x3ff0, 0x0071, 2, 0, 0x0010, 0x020d, 0x0010,
            ],
            vec![(Some("Ljava/lang/RuntimeException;"), 9)],
            5,
            8,
            ty,
        );
        // const-wide is initially J; a double parameter establishes D without
        // depending on literal inference in the handler probe.
        if ty == "D" {
            class.methods[0].parameters = vec!["D".into()];
            let code = class.methods[0].code.as_mut().unwrap();
            code.registers = 5;
            code.ins = 2;
            code.instructions.splice(0..5, [0x3004, 0, 0, 0, 0]);
        }
        let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        assert!(
            code.source.contains("catch (java.lang.RuntimeException"),
            "{}",
            code.source
        );
        assert_eq!(code.source.matches("sample.Effects.touch()").count(), 1);

        // Writing either half makes the handler-visible value mutable. Keep
        // rejecting it until wide exception snapshots themselves are supported.
        for write in [0x0016, 0x0116] {
            let original = class.methods[0].code.as_ref().unwrap().instructions.clone();
            let dex = class.methods[0].code.as_mut().unwrap();
            dex.registers = 5;
            dex.instructions.splice(5..5, [write, 42]);
            dex.try_regions[0].end += 2;
            dex.try_regions[0].catches =
                vec![(Some("Ljava/lang/RuntimeException;".into()), 11)].into();
            let error = native_java::render_method("sample.Effects", &class, &class.methods[0])
                .unwrap_err();
            assert!(
                error.to_string().contains("wide exception snapshots"),
                "{error:#}"
            );
            let dex = class.methods[0].code.as_mut().unwrap();
            dex.instructions = original;
            dex.try_regions[0].end -= 2;
            dex.try_regions[0].catches =
                vec![(Some("Ljava/lang/RuntimeException;".into()), 9)].into();
        }
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn wide_try_success_and_exception_paths_run_on_jvm() {
    use std::{fs, process::Command};
    let directory = std::env::temp_dir().join(format!("rdx-wide-try-jvm-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let mut source = String::from(
        "package sample; public class Effects { static boolean fail; static int calls;\n",
    );
    for (ty, name, producer) in [
        ("J", "longTest", "first"),
        ("D", "doubleTest", "firstDouble"),
    ] {
        // Protected invoke/result, normal wide return; catch produces zero.
        let mut class = fixture(
            vec![0x0071, 0, 0, 0x000b, 0x0010, 0x020d, 0x0016, 0, 0x0010],
            vec![(Some("Ljava/lang/RuntimeException;"), 5)],
            0,
            4,
            ty,
        );
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols.protos[0].0 = ty.into();
        symbols.strings[0] = producer.into();
        class.methods[0].name = name.into();
        let rendered =
            native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        source.push_str(&rendered.source);
    }
    source.push_str(r#"
static long first() { calls++; if (fail) throw new IllegalStateException(); return 0x123456789abcdef0L; }
static double firstDouble() { calls++; if (fail) throw new IllegalStateException(); return -0.0d; }
public static void main(String[] args) {
    if (longTest() != 0x123456789abcdef0L || calls != 1) throw new AssertionError("long success");
    if (Double.doubleToRawLongBits(doubleTest()) != 0x8000000000000000L || calls != 2) throw new AssertionError("double success");
    fail = true;
    if (longTest() != 0L || calls != 3) throw new AssertionError("long catch");
    if (Double.doubleToRawLongBits(doubleTest()) != 0L || calls != 4) throw new AssertionError("double catch");
}
}
"#);
    fs::write(directory.join("sample/Effects.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn android_api_checked_catch_uses_exact_declared_call() {
    let mut class = fixture(
        vec![0x106e, 0, 2, 0x000a, 0x000f, 0x010d, 0x0012, 0x000f],
        vec![(Some("Ljava/io/IOException;"), 5)],
        0,
        4,
        "I",
    );
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types[0] = "Ljava/io/InputStream;".into();
    symbols.strings[0] = "read".into();
    class.methods[0].parameters = vec!["Ljava/io/InputStream;".into()];
    class.methods[0].code.as_mut().unwrap().ins = 1;
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains("catch (java.io.IOException "),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches(".read()").count(), 1);
    // A method on the same class that does not declare IOException is not proof.
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "hashCode".into();
    let guarded = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(guarded.source.contains("instanceof java.io.IOException"));
    assert!(!guarded.source.contains("catch (java.io.IOException"));
}

fn nonvoid_cleanup_fixture() -> DexClass {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, 0x0071, 1, 0, 0x000a, 0x0071, 2, 0, 0x000f, 0x010d, 0x0071, 2, 0,
            0x0127, 0x010d, 0x0012, 0x000f,
        ],
        vec![],
        0,
        1,
        "I",
    );
    let outer: Arc<[(Option<Arc<str>>, u32)]> =
        vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 17)].into();
    class.methods[0].code.as_mut().unwrap().try_regions = vec![
        DexTryRegion {
            start: 0,
            end: 4,
            catches: outer.clone(),
        },
        DexTryRegion {
            start: 4,
            end: 8,
            catches: vec![(None, 12)].into(),
        },
        DexTryRegion {
            start: 8,
            end: 12,
            catches: outer.clone(),
        },
        DexTryRegion {
            start: 13,
            end: 17,
            catches: outer,
        },
    ];
    class.methods[0].code.as_mut().unwrap().tries = 4;
    class
}

#[test]
fn nested_cleanup_preserves_nonvoid_return() {
    let class = nonvoid_cleanup_fixture();
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("finally {"), "{}", code.source);
    assert_eq!(code.source.matches(".touch()").count(), 1);
    assert!(code.source.find("return ").unwrap() < code.source.find("finally {").unwrap());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn nonvoid_finally_return_and_throw_paths_run_on_jvm() {
    use std::{fs, process::Command};
    let class = nonvoid_cleanup_fixture();
    let method = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    let directory =
        std::env::temp_dir().join(format!("rdx-nonvoid-finally-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let source = format!(
        r#"package sample; public class Effects {{
static int fail, trace;
static int first() {{ trace = trace * 10 + 1; if (fail == 1) throw new IllegalStateException(); return 13; }}
static int second() {{ trace = trace * 10 + 2; if (fail == 2) throw new IllegalStateException(); return 27; }}
static void touch() {{ trace = trace * 10 + 3; if (fail == 3) throw new IllegalStateException(); }}
{}
public static void main(String[] args) {{
    for (fail = 0; fail < 4; fail++) {{
        trace = 0;
        int result = test();
        if (result != (fail == 0 ? 27 : 0)) throw new AssertionError("return " + fail);
        if (trace != (fail == 1 ? 1 : 123)) throw new AssertionError("effects " + fail + ": " + trace);
    }}
}}
}}"#,
        method.source
    );
    fs::write(directory.join("sample/Effects.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn narrowed_checked_catch_preserves_other_exceptions_on_jvm() {
    use std::{fs, process::Command};
    let mut class = fixture(
        vec![0x106e, 0, 2, 0x000a, 0x000f, 0x010d, 0x0012, 0x000f],
        vec![(Some("Ljava/io/EOFException;"), 5)],
        0,
        4,
        "I",
    );
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types[0] = "Ljava/io/InputStream;".into();
    symbols.strings[0] = "read".into();
    class.methods[0].parameters = vec!["Ljava/io/InputStream;".into()];
    class.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    class.methods[0].code.as_mut().unwrap().ins = 1;
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let method = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    let directory = std::env::temp_dir().join(format!("rdx-narrowed-catch-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let source = format!(
        r#"package sample; public class Effects {{
{}
static int mode;
public static void main(String[] args) throws Exception {{
    java.io.InputStream stream = new java.io.InputStream() {{
        public int read() throws java.io.IOException {{
            if (mode == 1) throw new java.io.EOFException();
            if (mode == 2) throw new java.io.IOException("other");
            return 7;
        }}
    }};
    if (test(stream) != 7) throw new AssertionError();
    mode = 1;
    if (test(stream) != 0) throw new AssertionError();
    mode = 2;
    try {{ test(stream); throw new AssertionError(); }}
    catch (java.io.IOException expected) {{ if (!"other".equals(expected.getMessage())) throw new AssertionError(); }}
}}
}}"#,
        method.source
    );
    fs::write(directory.join("sample/Effects.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "requires javac and java"]
fn erased_throws_dispatch_preserves_order_identity_and_unmatched_exceptions() {
    use std::{fs, process::Command};
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, 0x000f, 0x010d, 0x0013, 10, 0x000f, 0x010d, 0x0013, 20, 0x000f,
        ],
        vec![
            (Some("Ljava/io/EOFException;"), 5),
            (Some("Ljava/io/IOException;"), 9),
        ],
        0,
        4,
        "I",
    );
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(h)).unwrap();
    let dispatch = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    class.methods[0].name = "rethrow".into();
    let dex = class.methods[0].code.as_mut().unwrap();
    dex.instructions = vec![0x0071, 0, 0, 0x000a, 0x000f, 0x010d, 0x0127];
    dex.try_regions[0].catches = vec![(Some(Arc::from("Ljava/io/IOException;")), 5)].into();
    let rethrow = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    let source = format!(
        r#"package sample; public class Effects {{
static Throwable failure; static int calls;
@SuppressWarnings("unchecked") static <E extends Throwable> void raise(Throwable e) throws E {{ throw (E)e; }}
static int first() {{ calls++; if (failure!=null) Effects.<RuntimeException>raise(failure); return 7; }}
{dispatch}
{rethrow}
public static void main(String[] args) {{
 if (test()!=7 || calls!=1) throw new AssertionError();
 failure=new java.io.EOFException(); if(test()!=10 || calls!=2) throw new AssertionError("first matching handler");
 failure=new java.io.IOException(); if(test()!=20 || calls!=3) throw new AssertionError("super handler");
 for(Throwable e : new Throwable[] {{new IllegalStateException(),new AssertionError()}}) {{
  failure=e; try {{test();throw new AssertionError("swallowed");}} catch(Throwable actual) {{if(actual!=e)throw new AssertionError("identity");}}
 }}
 failure=new java.io.IOException();
 try {{rethrow();throw new AssertionError("swallowed");}}catch(Throwable actual){{if(actual!=failure)throw new AssertionError("rethrow identity");}}
}}
}}"#
    );
    let directory = std::env::temp_dir().join(format!("rdx-erased-throws-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    fs::write(directory.join("sample/Effects.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

fn terminal_result_reuses_handler_input_fixture(goto_tail: bool) -> DexClass {
    let mut class = fixture(
        if goto_tail {
            vec![
                0x1071, 0, 0, 0x000a, 0x0128, 0x000f, 0x1071, 1, 0, 0x000a, 0x000f,
            ]
        } else {
            vec![0x1071, 0, 0, 0x000a, 0x000f, 0x1071, 1, 0, 0x000a, 0x000f]
        },
        vec![(
            Some("Ljava/lang/AbstractMethodError;"),
            if goto_tail { 6 } else { 5 },
        )],
        0,
        4,
        "I",
    );
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[0] = ("I".into(), vec!["Ljava/lang/String;".into()]);
    class.methods[0].parameters = vec!["Ljava/lang/String;".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 1;
    code.ins = 1;
    code.outs = 1;
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(h)).unwrap();
    class
}

#[test]
fn terminal_result_can_reuse_exception_handler_argument_with_another_type() {
    for goto_tail in [false, true] {
        let class = terminal_result_reuses_handler_input_fixture(goto_tail);
        let rendered =
            native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        assert!(
            rendered
                .source
                .contains("catch (java.lang.AbstractMethodError"),
            "{}",
            rendered.source
        );
    }
}

#[test]
#[ignore = "requires javac and java"]
fn terminal_result_snapshot_preserves_normal_and_exception_values_on_jvm() {
    use std::{fs, process::Command};
    for goto_tail in [false, true] {
        let class = terminal_result_reuses_handler_input_fixture(goto_tail);
        let method =
            native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
        let source = format!(
            r#"package sample; public class Effects {{
static boolean fail; static String observed;
static int first(String s) {{ if(fail) throw new AbstractMethodError(); return s.length(); }}
static int second(String s) {{ observed=s;return 91; }}
{}
public static void main(String[] args) {{
 String s=new String("original");if(test(s)!=8 || observed!=null)throw new AssertionError();
 fail=true;if(test(s)!=91 || observed!=s)throw new AssertionError("handler lost original register");
}}
}}"#,
            method.source
        );
        let dir = std::env::temp_dir().join(format!(
            "rdx-terminal-snapshot-{}-{goto_tail}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("sample")).unwrap();
        fs::write(dir.join("sample/Effects.java"), source).unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Effects.java"]),
            ("java", vec!["-cp", ".", "sample.Effects"]),
        ] {
            let result = Command::new(program)
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

fn nested_guarded_probe_fixture() -> String {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, 0x000f, 0x010d, 0x0071, 2, 0, 0x0012, 0x000f, 0x020d, 0x0127,
        ],
        vec![(Some("Ljava/lang/RuntimeException;"), 5)],
        0,
        4,
        "I",
    );
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .try_regions
        .push(DexTryRegion {
            start: 6,
            end: 9,
            catches: vec![(Some(Arc::from("Ljava/io/IOException;")), 11)].into(),
        });
    class.methods[0].code.as_mut().unwrap().tries = 2;
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(h)).unwrap();
    native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn nested_guarded_probe_does_not_rewrite_outer_plain_catch_identity() {
    let source = nested_guarded_probe_fixture();
    assert!(
        !source.contains("throw caught0;"),
        "probe-local dispatch variable escaped: {source}"
    );
}

#[test]
#[ignore = "requires javac and java"]
fn nested_guarded_probe_preserves_outer_exception_identity_on_jvm() {
    use std::{fs, process::Command};
    let method = nested_guarded_probe_fixture();
    let source = format!(
        r#"package sample; public class Effects {{
static RuntimeException outer = new IllegalStateException("outer");
static Throwable inner;
@SuppressWarnings("unchecked") static <E extends Throwable> void raise(Throwable e) throws E {{ throw (E)e; }}
static int first() {{ throw outer; }}
static void touch() {{ if(inner!=null) Effects.<RuntimeException>raise(inner); }}
{method}
public static void main(String[] args) {{
 if(test()!=0)throw new AssertionError();
 inner=new java.io.IOException("inner");
 try {{test();throw new AssertionError("swallowed");}}catch(Throwable actual){{if(actual!=outer)throw new AssertionError("outer identity");}}
 inner=new AssertionError("unmatched");
 try {{test();throw new AssertionError("swallowed");}}catch(Throwable actual){{if(actual!=inner)throw new AssertionError("unmatched identity");}}
}}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-nested-probe-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Effects.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
