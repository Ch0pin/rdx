//! Numeric semantics checked entirely in Rust. Include the isolated emitter so
//! its API can stay private until it is consumed by the reconstruction pipeline.
#[path = "../src/native_java/numeric.rs"]
mod numeric;
use numeric::{Binary, Compare, Kind, Literal, Unary};

#[test]
fn raw_constants_require_correct_width_and_preserve_signed_bits() {
    assert_eq!(Literal::Bits32(u32::MAX).render(Kind::Int).unwrap(), "-1");
    assert_eq!(Literal::Bits64(u64::MAX).render(Kind::Long).unwrap(), "-1L");
    assert_eq!(
        Literal::Bits64(1 << 63).render(Kind::Long).unwrap(),
        "-9223372036854775808L"
    );
    assert!(Literal::Bits64(1).render(Kind::Int).is_err());
    assert!(Literal::Bits32(1).render(Kind::Double).is_err());
    for (kind, descriptor, width) in [
        (Kind::Int, "I", 1),
        (Kind::Long, "J", 2),
        (Kind::Float, "F", 1),
        (Kind::Double, "D", 2),
    ] {
        assert_eq!(kind.descriptor(), descriptor);
        assert_eq!(kind.width(), width);
    }
}
#[test]
fn floating_literals_roundtrip_finite_and_quiet_nan_patterns() {
    let mut state = 0x9e3779b97f4a7c15u64;
    for bits in [
        0,
        1,
        1 << 63,
        u64::MAX,
        0x7ff0000000000000,
        0xfff0000000000000,
    ]
    .into_iter()
    .chain((0..2048).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    })) {
        let double = f64::from_bits(bits);
        let rendered = Literal::Bits64(bits).render(Kind::Double);
        if double.is_nan() {
            if bits & 0x0008_0000_0000_0000 == 0 {
                assert!(rendered.is_err());
            } else {
                assert_eq!(
                    rendered.unwrap(),
                    format!("java.lang.Double.longBitsToDouble(0x{bits:016x}L)")
                );
            }
        } else if double.is_finite() {
            assert_eq!(
                rendered
                    .unwrap()
                    .strip_suffix('d')
                    .unwrap()
                    .parse::<f64>()
                    .unwrap()
                    .to_bits(),
                bits
            );
        } else {
            assert!(rendered.unwrap().contains("/ 0.0d"));
        }
        let bits = bits as u32;
        let float = f32::from_bits(bits);
        let rendered = Literal::Bits32(bits).render(Kind::Float);
        if float.is_nan() {
            if bits & 0x0040_0000 == 0 {
                assert!(rendered.is_err());
            } else {
                assert_eq!(
                    rendered.unwrap(),
                    format!("java.lang.Float.intBitsToFloat(0x{bits:08x})")
                );
            }
        } else if float.is_finite() {
            assert_eq!(
                rendered
                    .unwrap()
                    .strip_suffix('f')
                    .unwrap()
                    .parse::<f32>()
                    .unwrap()
                    .to_bits(),
                bits
            );
        } else {
            assert!(rendered.unwrap().contains("/ 0.0f"));
        }
    }
    assert_eq!(
        Literal::Bits32(1 << 31).render(Kind::Float).unwrap(),
        "-0.0f"
    );
    assert_eq!(
        Literal::Bits64(1 << 63).render(Kind::Double).unwrap(),
        "-0.0d"
    );
}

// Independent evaluator for the comparison ternary subset emitted by Java.
fn compare_expression(source: &str, left: f64, right: f64) -> i32 {
    fn strip(mut s: &str) -> &str {
        loop {
            if !s.starts_with('(') || !s.ends_with(')') {
                return s;
            }
            let mut depth = 0;
            if !s.char_indices().all(|(i, c)| {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                }
                depth != 0 || i == s.len() - 1
            }) {
                return s;
            }
            s = &s[1..s.len() - 1];
        }
    }
    let source = strip(source.trim());
    if let Ok(value) = source.parse() {
        return value;
    }
    let mut depth = 0;
    let question = source
        .char_indices()
        .find_map(|(i, c)| {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            (c == '?' && depth == 0).then_some(i)
        })
        .unwrap();
    let mut depth = 0;
    let colon = source[question + 1..]
        .char_indices()
        .find_map(|(i, c)| {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            (c == ':' && depth == 0).then_some(question + 1 + i)
        })
        .unwrap();
    let condition = &source[..question];
    let take = if condition.contains(" == ") {
        left == right
    } else if condition.contains(" > ") {
        left > right
    } else if condition.contains(" < ") {
        left < right
    } else {
        panic!("unexpected comparison {source}")
    };
    compare_expression(
        if take {
            &source[question + 1..colon]
        } else {
            &source[colon + 1..]
        },
        left,
        right,
    )
}
#[test]
fn floating_comparisons_match_dex_nan_bias_and_signed_zero() {
    let cases = [
        f64::NEG_INFINITY,
        -3.0,
        -0.0,
        0.0,
        2.0,
        f64::INFINITY,
        f64::NAN,
    ];
    for op in 0x2d..=0x30 {
        let spec = Compare::decode(op).unwrap();
        let expression = spec.expression("left", "right");
        for left in cases {
            for right in cases {
                let expected = match left.partial_cmp(&right) {
                    Some(std::cmp::Ordering::Less) => -1,
                    Some(std::cmp::Ordering::Equal) => 0,
                    Some(std::cmp::Ordering::Greater) => 1,
                    None if matches!(op, 0x2d | 0x2f) => -1,
                    None => 1,
                };
                assert_eq!(
                    compare_expression(&expression, left, right),
                    expected,
                    "op={op:x}, {left:?}, {right:?}"
                );
            }
        }
        assert_eq!(
            spec.input,
            if op <= 0x2e {
                Kind::Float
            } else {
                Kind::Double
            }
        );
    }
    assert_eq!(Compare::decode(0x31).unwrap().input, Kind::Long);
    assert!(Compare::decode(0x32).is_none());
}
#[test]
fn wide_arithmetic_uses_narrow_shift_distance_and_equivalent_twoaddr_specs() {
    for op in 0x90..=0xaf {
        assert_eq!(Binary::decode(op), Binary::decode(op + 0x20));
    }
    for op in 0x9b..=0xa5 {
        let spec = Binary::decode(op).unwrap();
        assert_eq!(spec.left, Kind::Long);
        assert_eq!(spec.result, Kind::Long);
        assert_eq!(spec.right, if op >= 0xa3 { Kind::Int } else { Kind::Long });
        assert_eq!(
            spec.expression("lhs", "rhs"),
            format!("(lhs) {} (rhs)", spec.operator)
        );
    }
    assert_eq!(Binary::decode(0xa5).unwrap().operator, ">>>");
    assert_eq!(Binary::decode(0xa4).unwrap().operator, ">>");
    assert!(Binary::decode(0xd0).is_none());
}
#[test]
fn numeric_casts_retain_java_conversion_semantics_and_narrow_types() {
    for (op, input, result, descriptor, expression) in [
        (0x81, Kind::Int, Kind::Long, "J", "(long) (value)"),
        (0x84, Kind::Long, Kind::Int, "I", "(int) (value)"),
        (0x87, Kind::Float, Kind::Int, "I", "(int) (value)"),
        (0x88, Kind::Float, Kind::Long, "J", "(long) (value)"),
        (0x8a, Kind::Double, Kind::Int, "I", "(int) (value)"),
        (0x8b, Kind::Double, Kind::Long, "J", "(long) (value)"),
        (0x8d, Kind::Int, Kind::Int, "B", "(byte) (value)"),
        (0x8e, Kind::Int, Kind::Int, "C", "(char) (value)"),
        (0x8f, Kind::Int, Kind::Int, "S", "(short) (value)"),
    ] {
        let spec = Unary::decode(op).unwrap();
        assert_eq!(
            (spec.input, spec.result, spec.result_descriptor),
            (input, result, descriptor)
        );
        assert_eq!(spec.expression("value"), expression);
    }
    for op in 0x7b..=0x8f {
        assert!(Unary::decode(op).is_some());
    }
    assert!(Unary::decode(0x90).is_none());
}

// Actual end-to-end method fixtures are enabled as the wide register frame is
// integrated; no external compiler or runtime is involved in these fixtures.
fn render_numeric(
    words: &[u16],
    parameters: &[&str],
    ret: &str,
    registers: u16,
) -> anyhow::Result<rdx::engine::DecompiledCode> {
    let mut class = rdx::native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    let method = &mut class.methods[0];
    method.name = "numeric".into();
    method.parameters = parameters.iter().map(|ty| (*ty).into()).collect();
    method.return_type = ret.into();
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = parameters
        .iter()
        .map(|ty| if matches!(*ty, "J" | "D") { 2 } else { 1 })
        .sum();
    code.instructions = words.to_vec();
    rdx::native_java::render_method("sample.Hello", &class, &class.methods[0])
}

#[test]
fn malformed_wide_frames_never_emit_plausible_java() {
    for words in [
        vec![0x1112, 0x0010], // narrow overwrite high half of p0
        vec![0x1012, 0x0010], // narrow overwrite low half of p0
        vec![0x1004, 0x0010], // move-wide reads high half
        vec![0x0001, 0x0010], // narrow move cannot consume wide head
    ] {
        assert!(
            render_numeric(&words, &["J"], "J", 2).is_err(),
            "accepted {words:x?}"
        );
    }
}

fn long_expression(source: &str, variables: &std::collections::HashMap<String, i64>) -> i64 {
    let mut source = source.trim();
    while source.starts_with('(') && source.ends_with(')') {
        let mut depth = 0;
        if !source.char_indices().all(|(i, c)| {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
            depth != 0 || i == source.len() - 1
        }) {
            break;
        }
        source = source[1..source.len() - 1].trim();
    }
    for operator in [
        " >>> ", " >> ", " << ", " + ", " - ", " * ", " / ", " % ", " & ", " | ", " ^ ",
    ] {
        if let Some((left, right)) = source.split_once(operator) {
            let left = long_expression(left, variables);
            let right = long_expression(right, variables);
            return match operator {
                " >>> " => ((left as u64) >> ((right as u32) & 63)) as i64,
                " >> " => left >> ((right as u32) & 63),
                " << " => left.wrapping_shl((right as u32) & 63),
                " + " => left.wrapping_add(right),
                " - " => left.wrapping_sub(right),
                " * " => left.wrapping_mul(right),
                " / " => left.wrapping_div(right),
                " % " => left.wrapping_rem(right),
                " & " => left & right,
                " | " => left | right,
                " ^ " => left ^ right,
                _ => unreachable!(),
            };
        }
    }
    if let Some(value) = source.strip_prefix('~') {
        return !long_expression(value, variables);
    }
    if let Some(value) = variables.get(source) {
        return *value;
    }
    source
        .trim_end_matches('L')
        .parse()
        .unwrap_or_else(|_| panic!("unknown long expression {source}"))
}
#[test]
fn emitted_long_operators_preserve_overflow_division_and_shift_masks() {
    for lhs in [i64::MIN, i64::MIN + 1, -7, -1, 0, 1, i64::MAX] {
        for rhs in [-65, -1, 1, 31, 32, 63, 64, 65] {
            let variables =
                std::collections::HashMap::from([("lhs".into(), lhs), ("rhs".into(), rhs)]);
            for (op, expected) in [
                (0x9b, lhs.wrapping_add(rhs)),
                (0x9c, lhs.wrapping_sub(rhs)),
                (0x9d, lhs.wrapping_mul(rhs)),
                (0x9e, lhs.wrapping_div(rhs)),
                (0x9f, lhs.wrapping_rem(rhs)),
                (0xa0, lhs & rhs),
                (0xa1, lhs | rhs),
                (0xa2, lhs ^ rhs),
                (0xa3, lhs.wrapping_shl((rhs as u32) & 63)),
                (0xa4, lhs >> ((rhs as u32) & 63)),
                (0xa5, ((lhs as u64) >> ((rhs as u32) & 63)) as i64),
            ] {
                let expression = Binary::decode(op).unwrap().expression("lhs", "rhs");
                assert_eq!(
                    long_expression(&expression, &variables),
                    expected,
                    "op={op:x}, lhs={lhs}, rhs={rhs}"
                );
            }
        }
    }
}

#[test]
fn actual_wide_parameters_returns_arithmetic_and_raw_constants_reconstruct() {
    for ty in ["J", "D"] {
        let code = render_numeric(&[0x0010], &[ty], ty, 2).unwrap();
        assert!(code.source.contains("return p0;"), "{}", code.source);
    }
    for (ty, op, text) in [("J", 0x9b, "long"), ("D", 0xab, "double")] {
        let code = render_numeric(&[op, 0x0200, 0x0010], &[ty, ty], ty, 4).unwrap();
        assert!(
            code.source.contains(&format!("{text} v0 = (p0) + (p1);")),
            "{}",
            code.source
        );
    }
    let code = render_numeric(&[0x00a6, 0x0100, 0x000f], &["F", "F"], "F", 2).unwrap();
    assert!(code.source.contains("float v0 = (p0) + (p1);"));
    for (ty, expected) in [("J", "4607182418800017408L"), ("D", "1.0d")] {
        let code = render_numeric(&[0x0018, 0, 0, 0, 0x3ff0, 0x0010], &[], ty, 2).unwrap();
        assert!(
            code.source.contains(&format!("return {expected};")),
            "{}",
            code.source
        );
    }
    let code = render_numeric(&[0x0014, 0, 0x3f80, 0x000f], &[], "F", 1).unwrap();
    assert!(code.source.contains("return 1.0f;"));
}
#[test]
fn actual_wide_overlapping_moves_read_before_pair_invalidation() {
    let code = render_numeric(&[0x0016, 42, 0x0104, 0x0110], &[], "J", 3).unwrap();
    assert!(code.source.contains("return 42L;"));
    // Two independent pairs: moving p0 into overlapping v1/v2 invalidates p1.
    assert!(render_numeric(&[0x0104, 0x0210], &["J", "J"], "J", 4).is_err());
    assert!(render_numeric(&[0x0016, 42, 0x0010], &[], "J", 1).is_err());
    // A typed long is not silently reinterpreted as double.
    assert!(render_numeric(&[0x0010], &["J"], "D", 2).is_err());
    assert!(render_numeric(&[0x000f], &["J"], "J", 2).is_err());
}
#[test]
fn actual_long_arithmetic_matches_independent_dex_values() {
    for op in 0x9b..=0xa5 {
        let parameters: &[&str] = if op >= 0xa3 { &["J", "I"] } else { &["J", "J"] };
        let register_count = if op >= 0xa3 { 3 } else { 4 };
        let source = render_numeric(&[op, 0x0200, 0x0010], parameters, "J", register_count)
            .unwrap()
            .source;
        for (lhs, rhs) in [(i64::MIN, -1), (-7, 65), (i64::MAX, 1)] {
            let mut variables =
                std::collections::HashMap::from([("p0".into(), lhs), ("p1".into(), rhs)]);
            let mut actual = None;
            for line in source.lines().map(str::trim) {
                if let Some((name, value)) = line
                    .strip_prefix("long ")
                    .and_then(|s| s.trim_end_matches(';').split_once(" = "))
                {
                    variables.insert(name.into(), long_expression(value, &variables));
                }
                if let Some(value) = line.strip_prefix("return ") {
                    actual = Some(long_expression(value.trim_end_matches(';'), &variables));
                }
            }
            let expected = match op {
                0x9b => lhs.wrapping_add(rhs),
                0x9c => lhs.wrapping_sub(rhs),
                0x9d => lhs.wrapping_mul(rhs),
                0x9e => lhs.wrapping_div(rhs),
                0x9f => lhs.wrapping_rem(rhs),
                0xa0 => lhs & rhs,
                0xa1 => lhs | rhs,
                0xa2 => lhs ^ rhs,
                0xa3 => lhs.wrapping_shl((rhs as u32) & 63),
                0xa4 => lhs >> ((rhs as u32) & 63),
                0xa5 => ((lhs as u64) >> ((rhs as u32) & 63)) as i64,
                _ => unreachable!(),
            };
            assert_eq!(actual, Some(expected), "{source}");
        }
    }
}
#[test]
fn actual_numeric_comparisons_conversions_and_control_flow_limits() {
    for (op, ty, regs) in [
        (0x2d, "F", 2),
        (0x2e, "F", 2),
        (0x2f, "D", 4),
        (0x30, "D", 4),
        (0x31, "J", 4),
    ] {
        let operands = if regs == 4 { 0x0200 } else { 0x0100 };
        let code = render_numeric(&[op, operands, 0x000f], &[ty, ty], "I", regs).unwrap();
        assert!(code.source.contains("?"));
        if ty != "J" {
            assert_eq!(
                compare_expression(
                    code.source
                        .split(" = ")
                        .nth(1)
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    f64::NAN,
                    1.0
                ),
                if matches!(op, 0x2d | 0x2f) { -1 } else { 1 }
            );
        }
    }
    for (op, src, dst, registers, ret) in [
        (0x81, "I", "J", 2, 0x0010),
        (0x84, "J", "I", 2, 0x000f),
        (0x8a, "D", "I", 2, 0x000f),
        (0x8b, "D", "J", 2, 0x0010),
    ] {
        // A narrow parameter lives at v1 when registers=2.
        let word = if src == "I" { 0x1000 | op } else { op };
        let code = render_numeric(&[word, ret], &[src], dst, registers).unwrap();
        assert!(code.source.contains("p0"));
    }
    let branch = render_numeric(&[0x0238, 3, 0x0010, 0x0010], &["J", "I"], "J", 3).unwrap();
    assert!(branch.source.contains("if (p1"), "{}", branch.source);
    // One arm destroys p0's upper word before the shared wide return.  The
    // join must fail closed instead of treating the tail as a Java local.
    assert!(
        render_numeric(
            &[0x0238, 4, 0x1112, 0x0228, 0x0000, 0x0010],
            &["J", "I"],
            "J",
            3
        )
        .is_err()
    );
}

#[test]
fn wide_field_invoke_and_array_paths_keep_exact_types_and_links() {
    use rdx::native_dex::{self, DexSymbols};
    use std::sync::Arc;
    for ty in ["J", "D"] {
        let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Helper;".into(), ty.into()],
            strings: vec!["value".into(), "identity".into()],
            fields: vec![(0, 1, 0)],
            methods: vec![(0, 0, 1)],
            protos: vec![(ty.into(), vec![ty.into()])],
            ..Default::default()
        });
        let method = &mut class.methods[0];
        method.name = "numeric".into();
        method.access_flags = 9;
        method.parameters = vec![ty.into()];
        method.return_type = ty.into();
        let code = method.code.as_mut().unwrap();
        code.registers = 2;
        code.ins = 2;
        code.outs = 2;
        code.instructions = vec![0x0068, 0, 0x0061, 0, 0x2071, 0, 0x0010, 0x000b, 0x0010];
        let rendered =
            rdx::native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
        for symbol in [
            format!("sample.Helper.value:{ty}"),
            format!("sample.Helper.identity({ty}){ty}"),
        ] {
            let link = rendered
                .links
                .iter()
                .find(|link| link.label == symbol)
                .unwrap();
            let text: String = rendered
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert!(text == "value" || text == "identity");
        }
        // Explicit invoke register list cannot supply a nonadjacent second word.
        class.methods[0].code.as_mut().unwrap().instructions[6] = 0;
        assert!(
            rdx::native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err()
        );
        let array = format!("[{ty}");
        // v0/v1=p0, v2=array, v3=index; store then load wide into v0/v1.
        let rendered = render_numeric(
            &[0x004c, 0x0302, 0x0045, 0x0302, 0x0010],
            &[ty, &array, "I"],
            ty,
            4,
        )
        .unwrap();
        assert!(rendered.source.contains("p1[p2] = p0;"));
    }
}
#[test]
fn all_wide_constant_formats_and_typed_nan_payloads() {
    for words in [
        vec![0x0016, 0xffff, 0x0010],
        vec![0x0017, 0xffff, 0xffff, 0x0010],
        vec![0x0018, 0xffff, 0xffff, 0xffff, 0xffff, 0x0010],
    ] {
        assert!(
            render_numeric(&words, &[], "J", 2)
                .unwrap()
                .source
                .contains("return -1L;")
        );
    }
    assert!(
        render_numeric(&[0x0019, 0x8000, 0x0010], &[], "D", 2)
            .unwrap()
            .source
            .contains("return -0.0d;")
    );
    for (words, ty, registers, expected) in [
        (
            vec![0x0018, 1, 0, 0, 0x7ff8, 0x0010],
            "D",
            2,
            "java.lang.Double.longBitsToDouble(0x7ff8000000000001L)",
        ),
        (
            vec![0x0014, 1, 0x7fc0, 0x000f],
            "F",
            1,
            "java.lang.Float.intBitsToFloat(0x7fc00001)",
        ),
        (
            vec![0x0014, 0xffff, 0xffff, 0x000f],
            "F",
            1,
            "java.lang.Float.intBitsToFloat(0xffffffff)",
        ),
    ] {
        assert!(
            render_numeric(&words, &[], ty, registers)
                .unwrap()
                .source
                .contains(expected)
        );
    }
    assert!(render_numeric(&[0x0018, 1, 0, 0, 0x7ff0, 0x0010], &[], "D", 2).is_err());
    assert!(render_numeric(&[0x0014, 1, 0x7f80, 0x000f], &[], "F", 1).is_err());
}

#[test]
fn actual_float_to_integer_casts_cover_nan_saturation_and_truncation() {
    for (op, src, dst) in [
        (0x87, "F", "I"),
        (0x88, "F", "J"),
        (0x8a, "D", "I"),
        (0x8b, "D", "J"),
    ] {
        let instruction = if src == "F" { 0x1000 | op } else { op };
        let ret = if dst == "J" { 0x0010 } else { 0x000f };
        let code = render_numeric(&[instruction, ret], &[src], dst, 2).unwrap();
        let expression = code
            .source
            .split(" = ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let cast = expression.split_whitespace().next().unwrap();
        assert_eq!(cast, if dst == "J" { "(long)" } else { "(int)" });
        for input in [
            f64::NAN,
            f64::NEG_INFINITY,
            f64::INFINITY,
            -0.0,
            0.0,
            -1.9,
            1.9,
            -2147483649.0,
            2147483648.0,
            -9223372036854775808.0,
            9223372036854775808.0,
        ] {
            let value = if src == "F" {
                input as f32 as f64
            } else {
                input
            };
            // Evaluate the emitted Java cast using Rust's matching saturating
            // float-to-integer conversion, then compare with explicit DEX rules.
            let actual = match cast {
                "(long)" => value as i64,
                "(int)" => (value as i32) as i64,
                _ => unreachable!(),
            };
            let (min, max) = if dst == "J" {
                (i64::MIN, i64::MAX)
            } else {
                (i32::MIN as i64, i32::MAX as i64)
            };
            let expected = if value.is_nan() {
                0
            } else if value >= max as f64 {
                max
            } else if value <= min as f64 {
                min
            } else {
                value.trunc() as i64
            };
            assert_eq!(actual, expected, "op={op:x} value={value:?}");
        }
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn generated_quiet_nan_returns_preserve_raw_bits_on_jvm() {
    use std::{fs, process::Command};
    let directory = std::env::temp_dir().join(format!("rdx-nan-jvm-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let mut source = String::from("public class NanReturns {\n");
    let mut checks = String::new();
    for (index, bits) in [
        0x7fc00000u32,
        0x7fc00001,
        0x7fffffff,
        0xffc00000,
        0xffc01234,
        0xffffffff,
    ]
    .into_iter()
    .enumerate()
    {
        let code = render_numeric(
            &[0x0014, bits as u16, (bits >> 16) as u16, 0x000f],
            &[],
            "F",
            1,
        )
        .unwrap();
        source.push_str(&code.source.replace("numeric(", &format!("f{index}(")));
        checks.push_str(&format!("if (Float.floatToRawIntBits(f{index}()) != 0x{bits:08x}) throw new AssertionError(\"float {index}\");\n"));
    }
    for (index, bits) in [
        0x7ff8000000000000u64,
        0x7ff8000000000001,
        0x7fffffffffffffff,
        0xfff8000000000000,
        0xfff8123456789abc,
        0xffffffffffffffff,
    ]
    .into_iter()
    .enumerate()
    {
        let code = render_numeric(
            &[
                0x0018,
                bits as u16,
                (bits >> 16) as u16,
                (bits >> 32) as u16,
                (bits >> 48) as u16,
                0x0010,
            ],
            &[],
            "D",
            2,
        )
        .unwrap();
        source.push_str(&code.source.replace("numeric(", &format!("d{index}(")));
        checks.push_str(&format!("if (Double.doubleToRawLongBits(d{index}()) != 0x{bits:016x}L) throw new AssertionError(\"double {index}\");\n"));
    }
    source.push_str(&format!(
        "public static void main(String[] args) {{\n{checks}}}\n}}\n"
    ));
    fs::write(directory.join("NanReturns.java"), source).unwrap();
    for (program, arguments) in [
        ("javac", vec!["NanReturns.java"]),
        ("java", vec!["-cp", ".", "NanReturns"]),
    ] {
        let output = Command::new(program)
            .args(arguments)
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
