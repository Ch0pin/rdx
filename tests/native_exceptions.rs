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
                regs[a] = Some(99);
                pc += 2;
            }
            0x70 | 0x71 => {
                let method = class.symbols.methods[code.instructions[pc + 1] as usize];
                let name = &class.symbols.strings[method.2 as usize];
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
            let mut depth = 0;
            let mut start = 0;
            for (offset, ch) in arguments.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        expression(arguments[start..offset].trim(), locals, fail, calls)?;
                        start = offset + 1;
                    }
                    _ => {}
                }
            }
            if !arguments.is_empty() {
                expression(arguments[start..].trim(), locals, fail, calls)?;
            }
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
    let error =
        native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap_err();
    assert!(
        error.to_string().contains("reorder class resolution"),
        "{error}"
    );
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
fn review_checked_catch_requires_a_potentially_throwing_protected_body() {
    let class = fixture(
        vec![0x0012, 0x000f, 0x1012, 0x000f],
        vec![(Some("Ljava/io/IOException;"), 2)],
        0,
        2,
        "I",
    );
    let rendered = native_java::render_method("sample.Effects", &class, &class.methods[0]);
    assert!(
        rendered.is_err(),
        "checked catch cannot compile: {}",
        rendered.unwrap().source
    );
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
