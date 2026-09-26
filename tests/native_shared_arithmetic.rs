//! G01-B-arithmetic: all 109 encodings through the public renderer.
//! References below are deliberately independent of production numeric decoders.
use rdx::{native_dex, native_java};
use std::{fs, process::Command};

struct Case {
    name: String,
    opcode: u16,
    parameters: Vec<&'static str>,
    result: &'static str,
    words: Vec<u16>,
    expected: String,
}
fn width(ty: &str) -> u16 {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn ret(ty: &str) -> u16 {
    if width(ty) == 2 { 0x10 } else { 0x0f }
}
fn render(case: &Case) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = case.name.as_str().into();
    method.access_flags = 9;
    method.parameters = case.parameters.iter().copied().map(Into::into).collect();
    method.return_type = case.result.into();
    let code = method.code.as_mut().unwrap();
    code.ins = case.parameters.iter().map(|ty| width(ty)).sum();
    code.registers = 2 + code.ins;
    code.outs = 0;
    code.instructions = case.words.clone();
    Ok(native_java::render_method("sample.Arithmetic", &class, &class.methods[0])?.source)
}
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (opcode, ty, nan) in [
        (0x2d, "F", -1),
        (0x2e, "F", 1),
        (0x2f, "D", -1),
        (0x30, "D", 1),
        (0x31, "J", 0),
    ] {
        let expected = if ty == "J" {
            "Long.compare(x, y)".into()
        } else {
            let class = if ty == "F" { "Float" } else { "Double" };
            format!(
                "({class}.isNaN(x) || {class}.isNaN(y)) ? {nan} : (x == y ? 0 : (x < y ? -1 : 1))"
            )
        };
        cases.push(Case {
            name: format!("op{opcode:02x}"),
            opcode,
            parameters: vec![ty, ty],
            result: "I",
            words: vec![opcode, ((2 + width(ty)) << 8) | 2, 0x000f],
            expected,
        });
    }
    // DEX 12x: destination v0, source v2; the two local words allow wide outputs.
    for (index, (input, result, expression)) in [
        ("I", "I", "-x"),
        ("I", "I", "~x"),
        ("J", "J", "-x"),
        ("J", "J", "~x"),
        ("F", "F", "-x"),
        ("D", "D", "-x"),
        ("I", "J", "(long)x"),
        ("I", "F", "(float)x"),
        ("I", "D", "(double)x"),
        ("J", "I", "(int)x"),
        ("J", "F", "(float)x"),
        ("J", "D", "(double)x"),
        ("F", "I", "(int)x"),
        ("F", "J", "(long)x"),
        ("F", "D", "(double)x"),
        ("D", "I", "(int)x"),
        ("D", "J", "(long)x"),
        ("D", "F", "(float)x"),
        ("I", "B", "(byte)x"),
        ("I", "C", "(char)x"),
        ("I", "S", "(short)x"),
    ]
    .into_iter()
    .enumerate()
    {
        let opcode = 0x7b + index as u16;
        cases.push(Case {
            name: format!("op{opcode:02x}"),
            opcode,
            parameters: vec![input],
            result,
            words: vec![0x2000 | opcode, ret(result)],
            expected: expression.into(),
        });
    }
    for (base, ty, operators) in [
        (
            0x90,
            "I",
            &["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"][..],
        ),
        (
            0x9b,
            "J",
            &["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"][..],
        ),
        (0xa6, "F", &["+", "-", "*", "/", "%"][..]),
        (0xab, "D", &["+", "-", "*", "/", "%"][..]),
    ] {
        for (index, operator) in operators.iter().enumerate() {
            let right_ty = if index >= 8 { "I" } else { ty };
            let right = 2 + width(ty);
            for twoaddr in [false, true] {
                let opcode = base + index as u16 + if twoaddr { 0x20 } else { 0 };
                let words = if twoaddr {
                    vec![(right << 12) | 0x0200 | opcode, 0x0200 | ret(ty)]
                } else {
                    vec![opcode, (right << 8) | 2, ret(ty)]
                };
                cases.push(Case {
                    name: format!("op{opcode:02x}"),
                    opcode,
                    parameters: vec![ty, right_ty],
                    result: ty,
                    words,
                    expected: format!("x {operator} y"),
                });
            }
        }
    }
    for opcode in 0xd0..=0xe2 {
        let lit8 = opcode >= 0xd8;
        let index = opcode - if lit8 { 0xd8 } else { 0xd0 };
        let operator = ["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"][index as usize];
        for literal in [-7i16, if lit8 { -128 } else { -32768 }] {
            let words = if lit8 {
                vec![opcode, ((literal as i8 as u8 as u16) << 8) | 2, 0x000f]
            } else {
                vec![0x2000 | opcode, literal as u16, 0x000f]
            };
            let expected = if index == 1 {
                format!("{literal} - x")
            } else {
                format!("x {operator} ({literal})")
            };
            cases.push(Case {
                name: format!("op{opcode:02x}n{}", -i32::from(literal)),
                opcode,
                parameters: vec!["I"],
                result: "I",
                words,
                expected,
            });
        }
    }
    cases
}

#[test]
fn all_arithmetic_encodings_render_through_public_api() {
    let cases = cases();
    assert_eq!(cases.len(), 128); // 109 encodings plus a second signed literal per encoding.
    let unique = cases
        .iter()
        .map(|c| c.opcode)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 109);
    for case in cases {
        let source = render(&case).unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(source.contains("return "), "{}: {source}", case.name);
        assert!(!source.contains(".method"), "{}: {source}", case.name);
        if !(0x2d..=0x31).contains(&case.opcode) {
            // Parentheses and spacing are renderer choices; operand order,
            // operator, signed literals and conversion types are semantics.
            let compact = |text: &str| {
                text.chars()
                    .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')'))
                    .collect::<String>()
            };
            let reference = case.expected.replace('x', "p0").replace(" y", " p1");
            assert!(
                compact(&source).contains(&compact(&reference)),
                "{} lost its reference expression {}: {source}",
                case.name,
                case.expected
            );
        }
    }
}

fn java_type(ty: &str) -> &'static str {
    match ty {
        "I" => "int",
        "J" => "long",
        "F" => "float",
        "D" => "double",
        "B" => "byte",
        "C" => "char",
        "S" => "short",
        _ => unreachable!(),
    }
}
fn values(ty: &str) -> &'static str {
    match ty {
        "I" => {
            "new int[]{Integer.MIN_VALUE, Integer.MAX_VALUE, -1, 0, 1, -65, -33, 31, 32, 33, 63, 64, 65, 128, 65535}"
        }
        "J" => {
            "new long[]{Long.MIN_VALUE, Long.MAX_VALUE, -1L, 0L, 1L, 0x123456789abcdef0L, -65L, 33L, 65L}"
        }
        "F" => {
            "new float[]{Float.NaN, Float.NEGATIVE_INFINITY, Float.POSITIVE_INFINITY, -0.0f, 0.0f, -1.75f, 1.75f, Float.MAX_VALUE, Float.MIN_VALUE, 0x1p31f, -0x1p31f, 0x1p63f}"
        }
        "D" => {
            "new double[]{Double.NaN, Double.NEGATIVE_INFINITY, Double.POSITIVE_INFINITY, -0.0d, 0.0d, -1.75d, 1.75d, Double.MAX_VALUE, Double.MIN_VALUE, 0x1p31, -0x1p31, 0x1p63, -0x1p63}"
        }
        _ => unreachable!(),
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn arithmetic_jvm_matches_independent_boundary_references() {
    let mut methods = String::new();
    let mut checks = String::new();
    for case in cases() {
        methods.push_str(&render(&case).unwrap());
        let parameters = case
            .parameters
            .iter()
            .zip(["x", "y"])
            .map(|(ty, name)| format!("{} {name}", java_type(ty)))
            .collect::<Vec<_>>()
            .join(", ");
        methods.push_str(&format!(
            "\nstatic {} ref{}({parameters}) {{ return {}; }}\n",
            java_type(case.result),
            case.name,
            case.expected
        ));
        let args = if case.parameters.len() == 2 {
            "x, y"
        } else {
            "x"
        };
        let mut loop_head = format!(
            "for ({} x : {}) {{",
            java_type(case.parameters[0]),
            values(case.parameters[0])
        );
        if case.parameters.len() == 2 {
            loop_head.push_str(&format!(
                "for ({} y : {}) {{",
                java_type(case.parameters[1]),
                values(case.parameters[1])
            ));
        }
        let equal = match case.result {
            "F" => "Float.floatToIntBits(actual) == Float.floatToIntBits(expected)",
            "D" => "Double.doubleToLongBits(actual) == Double.doubleToLongBits(expected)",
            _ => "actual == expected",
        };
        checks.push_str(&format!("{loop_head}\n{} expected;\ntry {{ expected = ref{}({args}); }} catch (ArithmeticException ex) {{\n  try {{ {}({args}); }} catch (ArithmeticException got) {{ continue; }}\n  throw new AssertionError(\"{} missed divide by zero\");\n}}\n{} actual = {}({args});\nif (!({equal})) throw new AssertionError(\"{}: \" + actual + \" expected \" + expected);\n{}\n", java_type(case.result), case.name, case.name, case.name, java_type(case.result), case.name, case.name, "}".repeat(case.parameters.len())));
    }
    let java = format!(
        "package sample; public class Arithmetic {{\n{methods}\npublic static void main(String[] args) {{\n{checks}\n}}\n}}"
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-arithmetic-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Arithmetic.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/Arithmetic.java"),
        ("java", "sample.Arithmetic"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\nFixture source: {}",
            String::from_utf8_lossy(&output.stderr),
            dir.join("sample/Arithmetic.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
