//! G01-B-start: public-renderer and JVM checks for shared move/constant operands.
use rdx::{native_dex, native_java};
use std::{fs, process::Command};

struct Case {
    name: String,
    words: Vec<u16>,
    registers: u16,
    parameter: Option<&'static str>,
    result: &'static str,
    expected: &'static str,
}

fn render(case: &Case) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = case.name.as_str().into();
    method.access_flags = 9;
    method.parameters = case.parameter.into_iter().map(Into::into).collect();
    method.return_type = case.result.into();
    let code = method.code.as_mut().unwrap();
    code.registers = case.registers;
    code.ins = match case.parameter {
        None => 0,
        Some("J" | "D") => 2,
        Some(_) => 1,
    };
    code.outs = 0;
    code.instructions = case.words.clone();
    Ok(native_java::render_method("sample.Hello", &class, &class.methods[0])?.source)
}

fn moves() -> Vec<Case> {
    let mut cases = Vec::new();
    for (kind, ty, base, ret, width) in [
        ("Int", "I", 0x01u16, 0x0fu16, 1),
        ("Long", "J", 0x04, 0x10, 2),
        ("Object", "Ljava/lang/Object;", 0x07, 0x11, 1),
    ] {
        let overwrite = if width == 2 {
            vec![0x2000 | base, 0x0216, 0, ret]
        } else {
            vec![0x2000 | base, 0x0212, ret]
        };
        cases.push(Case {
            name: format!("snapshot{kind}"),
            words: overwrite,
            registers: 2 + width,
            parameter: Some(ty),
            result: ty,
            expected: "",
        });
        // The /16 form really uses a destination >255, then copies back so
        // return's eight-bit operand can address the result.
        for (form, registers, words) in [
            ("Compact", 2 + width, vec![0x2000 | base, ret]),
            ("From16", 299 + width, vec![base + 1, 299, ret]),
            (
                "Both16",
                299 + width,
                vec![base + 2, 260, 299, base + 1, 260, ret],
            ),
        ] {
            cases.push(Case {
                name: format!("move{kind}{form}"),
                words,
                registers,
                parameter: Some(ty),
                result: ty,
                expected: "",
            });
        }
    }
    // DEX explicitly permits overlapping wide source/destination pairs.
    cases.push(Case {
        name: "wideLeft".into(),
        words: vec![0x1004, 0x0010],
        registers: 3,
        parameter: Some("J"),
        result: "J",
        expected: "",
    });
    cases.push(Case {
        name: "wideRight".into(),
        words: vec![0x2004, 0x0104, 0x0110],
        registers: 4,
        parameter: Some("J"),
        result: "J",
        expected: "",
    });
    cases
}

fn constants() -> Vec<Case> {
    [
        ("smallSigned", vec![0x8012], "I", "-8"),
        ("shortSigned", vec![0x0013, 0x8001], "I", "-32767"),
        (
            "fullSigned",
            vec![0x0014, 0x4567, 0x8123],
            "I",
            "0x81234567",
        ),
        ("highSigned", vec![0x0015, 0x8123], "I", "0x81230000"),
        ("wideShort", vec![0x0016, 0x8001], "J", "-32767L"),
        (
            "wideInt",
            vec![0x0017, 0x4567, 0x8123],
            "J",
            "(long)0x81234567",
        ),
        (
            "wideFull",
            vec![0x0018, 0xcdef, 0x89ab, 0x4567, 0x8123],
            "J",
            "0x8123456789abcdefL",
        ),
        ("wideHigh", vec![0x0019, 0x8123], "J", "0x8123000000000000L"),
        ("floatNegativeZero", vec![0x0015, 0x8000], "F", "0x80000000"),
        (
            "doubleNegativeZero",
            vec![0x0019, 0x8000],
            "D",
            "0x8000000000000000L",
        ),
        (
            "floatQuietNan",
            vec![0x0014, 0x2345, 0x7fc1],
            "F",
            "0x7fc12345",
        ),
        (
            "doubleQuietNan",
            vec![0x0018, 0xcdef, 0x89ab, 0x4567, 0x7ff8],
            "D",
            "0x7ff8456789abcdefL",
        ),
    ]
    .into_iter()
    .map(|(name, mut words, result, expected)| {
        words.push(if matches!(result, "J" | "D") {
            0x0010
        } else {
            0x000f
        });
        Case {
            name: name.into(),
            words,
            registers: 2,
            parameter: None,
            result,
            expected,
        }
    })
    .collect()
}

#[test]
fn every_move_encoding_and_overlapping_wide_copy_reconstructs() {
    for case in moves() {
        let source = render(&case).unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(source.contains("return "), "{}: {source}", case.name);
    }
}

#[test]
fn every_numeric_constant_encoding_and_raw_float_payload_reconstructs() {
    for case in constants() {
        let source = render(&case).unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(source.contains("return "), "{}: {source}", case.name);
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn shared_operands_jvm_preserves_identity_signed_bits_and_overlapping_wide_values() {
    let cases: Vec<_> = moves().into_iter().chain(constants()).collect();
    let mut body = String::new();
    let mut checks = String::new();
    for case in &cases {
        body.push_str(&render(case).unwrap());
        let invocation = format!(
            "{}({})",
            case.name,
            if case.parameter.is_some() {
                "value"
            } else {
                ""
            }
        );
        match case.parameter {
            Some("I") => checks.push_str(&format!("for (int value : new int[] {{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}}) if ({invocation} != value) throw new AssertionError(\"{}\");\n", case.name)),
            Some("J") => checks.push_str(&format!("for (long value : new long[] {{Long.MIN_VALUE, -1L, 0L, 1L, 0x123456789abcdefL, Long.MAX_VALUE}}) if ({invocation} != value) throw new AssertionError(\"{}\");\n", case.name)),
            Some(_) => checks.push_str(&format!("for (Object value : new Object[] {{null, new Object(), \"identity\"}}) if ({invocation} != value) throw new AssertionError(\"{}\");\n", case.name)),
            None => {
                let actual = match case.result {
                    "F" => format!("Float.floatToRawIntBits({invocation})"),
                    "D" => format!("Double.doubleToRawLongBits({invocation})"),
                    _ => invocation,
                };
                checks.push_str(&format!("if ({actual} != {}) throw new AssertionError(\"{}\");\n", case.expected, case.name));
            }
        }
    }
    let java = format!(
        "public class OperandChecks {{\n{body}\n public static void main(String[] args) {{\n{checks}\n}}\n}}"
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-operands-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("OperandChecks.java"), &java).unwrap();
    for (program, argument) in [("javac", "OperandChecks.java"), ("java", "OperandChecks")] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
