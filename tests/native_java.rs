//! Native-only reconstruction regressions. These tests consume committed DEX
//! bytes or synthetic instruction fixtures; no Java runtime/compiler is used.
use rdx::{
    native_dex::{self, DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture_class(descriptor: &str) -> DexClass {
    native_dex::parse(include_bytes!("fixtures/navigation.dex"))
        .unwrap()
        .classes
        .into_iter()
        .find(|class| class.descriptor.as_ref() == descriptor)
        .unwrap()
}
fn synthetic(instructions: &[u16]) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: Arc::from("Lsample/Arithmetic;"),
        superclass: Some(Arc::from("Ljava/lang/Object;")),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: Arc::from("Lsample/Arithmetic;"),
            name: Arc::from("calculate"),
            return_type: Arc::from("I"),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 2,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: instructions.to_vec(),
                offset: 0,
            }),
        }],
    }
}
fn span(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}

#[test]
fn hello_produces_native_java_with_exact_declaration_spans() {
    let dex = native_dex::parse(include_bytes!("fixtures/hello.dex")).unwrap();
    let code = native_java::render("sample.Hello", &dex.classes[0]).unwrap();
    assert!(code.source.contains("class Hello"), "{}", code.source);
    assert!(code.source.contains("int answer()"), "{}", code.source);
    assert!(code.source.contains("return 42;"), "{}", code.source);
    assert!(!code.source.contains(".method"));
    let declaration = code
        .definitions
        .iter()
        .find(|d| d.kind == "method")
        .unwrap();
    assert_eq!(
        span(&code.source, declaration.start, declaration.end),
        "answer"
    );
    assert!(code.links.iter().any(|link| link.start == declaration.start
        && link.end == declaration.end
        && link.label == "sample.Hello.answer()I"));
}

#[test]
fn real_fixture_integer_method_uses_input_parameter_and_multiplication() {
    let class = fixture_class("Lsample/Target;");
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "doubleValue" && m.return_type.as_ref() == "I")
        .unwrap();
    let code = native_java::render_method("sample.Target", &class, method).unwrap();
    assert!(
        code.source.contains("doubleValue(int p0)"),
        "{}",
        code.source
    );
    assert!(code.source.contains("p0 * (2)"), "{}", code.source);
    assert!(code.source.contains("return "), "{}", code.source);
    assert!(!code.source.contains("mul-int"));
}

#[test]
fn real_fixture_constructor_preserves_super_before_field_assignment() {
    let class = fixture_class("Lsample/Target;");
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "<init>")
        .unwrap();
    let code = native_java::render_method("sample.Target", &class, method).unwrap();
    let super_position = code.source.find("super();").unwrap();
    let field = code
        .links
        .iter()
        .find(|l| l.label == "sample.Target.value:I")
        .unwrap();
    assert!(super_position < code.source.char_indices().nth(field.start).unwrap().0);
    assert_eq!(span(&code.source, field.start, field.end), "value");
    assert!(code.source.contains("= 3;"), "{}", code.source);
}

#[test]
fn real_fixture_static_call_preserves_overload_identity() {
    let class = fixture_class("Lsample/Caller;");
    for (name, symbol) in [
        ("external", "java.lang.Math.abs(I)I"),
        ("local", "sample.Caller.helper()I"),
    ] {
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == name)
            .unwrap();
        let code = native_java::render_method("sample.Caller", &class, method).unwrap();
        let link = code.links.iter().find(|l| l.label == symbol).unwrap();
        assert_eq!(
            span(&code.source, link.start, link.end),
            if name == "external" { "abs" } else { "helper" }
        );
        assert!(code.source.contains("return "));
    }
}

#[test]
fn unsupported_control_flow_and_opaque_metadata_are_not_claimed_as_java() {
    // goto +0 and an exception-bearing method must never become an unconditional return.
    for instructions in [vec![0x0028, 0x0012, 0x000f], vec![0x0038, 2, 0x000f]] {
        let class = synthetic(&instructions);
        assert!(native_java::render("sample.Arithmetic", &class).is_err());
    }
    let mut class = synthetic(&[0x0012, 0x000f]);
    class.methods[0].code.as_mut().unwrap().tries = 1;
    assert!(native_java::render("sample.Arithmetic", &class).is_err());
    class.methods[0].code.as_mut().unwrap().tries = 0;
    class.annotations_offset = 1;
    assert!(native_java::render("sample.Arithmetic", &class).is_err());
    class.annotations_offset = 0;
    class.static_values_offset = 1;
    assert!(native_java::render("sample.Arithmetic", &class).is_err());
}

#[test]
fn malformed_registers_and_incomplete_instructions_do_not_emit_plausible_java() {
    for instructions in [
        vec![0x0013],
        vec![0x030f],
        vec![0x000f],
        vec![0x0012, 0x000f, 0x1012],
    ] {
        let class = synthetic(&instructions);
        assert!(
            native_java::render("sample.Arithmetic", &class).is_err(),
            "unexpected reconstruction of {instructions:04x?}"
        );
    }
}

// Evaluate only the tiny Java integer subset used in these arithmetic fixtures.
// This is independent of the DEX decoder and catches operand reversal, signed
// literals, register aliasing and Java's masked shift counts without a JVM.
fn evaluate_integer_method(source: &str, input: i32) -> i32 {
    let mut variables = std::collections::HashMap::from([("p0".to_owned(), input)]);
    fn operand(word: &str, vars: &std::collections::HashMap<String, i32>) -> i32 {
        let word = word.trim_matches(['(', ')', ';']);
        word.parse()
            .unwrap_or_else(|_| *vars.get(word).expect("known Java variable"))
    }
    for line in source.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("return ") {
            return operand(value, &variables);
        }
        if let Some(declaration) = line.strip_prefix("int ")
            && let Some((name, expression)) = declaration.split_once(" = ")
        {
            let parts: Vec<_> = expression
                .trim_end_matches(';')
                .split_whitespace()
                .collect();
            let left = operand(parts[0], &variables);
            let value = if parts.len() == 1 {
                left
            } else {
                let right = operand(parts[2], &variables);
                match parts[1] {
                    "+" => left.wrapping_add(right),
                    "-" => left.wrapping_sub(right),
                    "*" => left.wrapping_mul(right),
                    "<<" => left.wrapping_shl(right as u32),
                    ">>" => left.wrapping_shr(right as u32),
                    ">>>" => ((left as u32).wrapping_shr(right as u32)) as i32,
                    other => panic!("unhandled Java expression {other}"),
                }
            };
            variables.insert(name.to_owned(), value);
        }
    }
    panic!("no Java return in {source}");
}

#[test]
fn integer_lowering_preserves_dex_wrapping_and_reverse_subtract_semantics() {
    type ArithmeticCase = (Vec<u16>, fn(i32) -> i32);
    let cases: [ArithmeticCase; 3] = [
        (vec![0x00da, 0x0200, 0x000f], |x| x.wrapping_mul(2)),
        (vec![0x00d9, 0xfd00, 0x000f], |x| (-3_i32).wrapping_sub(x)),
        (vec![0x00e2, 0x2300, 0x000f], |x| ((x as u32) >> 3) as i32),
    ];
    for (instructions, expected) in cases {
        let mut class = synthetic(&instructions);
        let method = &mut class.methods[0];
        method.parameters = vec![Arc::from("I")];
        let code = method.code.as_mut().unwrap();
        code.registers = 1;
        code.ins = 1;
        let java = native_java::render("sample.Arithmetic", &class).unwrap();
        for input in [i32::MIN, i32::MAX, -33, -1, 0, 1, 42] {
            assert_eq!(
                evaluate_integer_method(&java.source, input),
                expected(input),
                "input={input}, source={}",
                java.source
            );
        }
    }
}

#[test]
fn short_field_names_link_only_the_operand_not_owner_or_local_names() {
    let mut class = synthetic(&[0x0060, 0, 0x000f]); // sget v0, field@0; return v0
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["a".into()],
        types: vec![Arc::from("Lsample/Arithmetic;"), Arc::from("I")],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    let links: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label == "sample.Arithmetic.a:I")
        .collect();
    assert_eq!(links.len(), 1, "{}", code.source);
    let link = links[0];
    assert_eq!(span(&code.source, link.start, link.end), "a");
    assert_eq!(code.source.chars().nth(link.start - 1), Some('.'));
}

#[test]
fn invalid_return_categories_are_rejected_and_wide_parameters_are_preserved() {
    for instructions in [
        vec![0x1012, 0x0011], // return-object on int
        vec![0x1012, 0x0010],
    ] {
        // return-wide on int
        let class = synthetic(&instructions);
        assert!(native_java::render("sample.Arithmetic", &class).is_err());
    }
    let mut class = synthetic(&[0x0010]);
    class.methods[0].parameters = vec![Arc::from("J")];
    class.methods[0].return_type = Arc::from("J");
    class.methods[0].code.as_mut().unwrap().ins = 2;
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    assert!(code.source.contains("return p0;"), "{}", code.source);
    // A wide return starting at the pair's high word remains invalid.
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x0110];
    assert!(native_java::render("sample.Arithmetic", &class).is_err());
}

#[test]
fn null_invocation_argument_keeps_declared_overload_type() {
    let mut class = synthetic(&[0x0012, 0x1071, 0, 0, 0x000e]);
    class.methods[0].return_type = Arc::from("V");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["accept".into()],
        types: vec![Arc::from("Lsample/Overloads;")],
        protos: vec![(Arc::from("V"), vec![Arc::from("Ljava/lang/String;")])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    assert!(
        code.source.contains("accept(((java.lang.String) null))"),
        "{}",
        code.source
    );
}

#[test]
fn widened_invocation_argument_keeps_declared_overload_type() {
    let mut class = synthetic(&[0x1071, 0, 1, 0x000e]);
    class.methods[0].return_type = Arc::from("V");
    class.methods[0].parameters = vec![Arc::from("B")];
    class.methods[0].code.as_mut().unwrap().ins = 1;
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["accept".into()],
        types: vec![Arc::from("Lsample/Overloads;")],
        protos: vec![(Arc::from("V"), vec![Arc::from("I")])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    assert!(
        code.source.contains("accept(((int) p0))"),
        "{}",
        code.source
    );
}

#[test]
fn invalid_member_names_alias_declarations_and_operands_but_keep_raw_navigation() {
    let mut class = synthetic(&[0x0071, 0, 0, 0x000e]); // invoke-static {}, method@0
    class.methods[0].name = Arc::from("call-me");
    class.methods[0].return_type = Arc::from("V");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["do-work".into(), "bad-field".into()],
        types: vec![Arc::from("Lsample/Arithmetic;"), Arc::from("I")],
        protos: vec![(Arc::from("V"), vec![])],
        fields: vec![(0, 1, 1)],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class.fields.push(native_dex::DexField {
        declaring_type: Arc::from("Lsample/Arithmetic;"),
        name: Arc::from("bad-field"),
        field_type: Arc::from("I"),
        access_flags: 9,
        is_static: true,
    });
    class.methods.push(DexMethod {
        declaring_type: Arc::from("Lsample/Arithmetic;"),
        name: Arc::from("read-field"),
        return_type: Arc::from("I"),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x0060, 0, 0x000f], // sget v0, field@0; return v0
            offset: 0,
        }),
    });
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    assert!(
        code.source.contains("int _rdx_6261642d6669656c64;"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("void _rdx_63616c6c2d6d65()"),
        "{}",
        code.source
    );
    assert!(
        code.source
            .contains("sample.Arithmetic._rdx_646f2d776f726b();"),
        "{}",
        code.source
    );
    assert!(
        code.source
            .contains("sample.Arithmetic._rdx_6261642d6669656c64"),
        "{}",
        code.source
    );
    let declaration = code
        .definitions
        .iter()
        .find(|definition| definition.kind == "method")
        .unwrap();
    assert_eq!(declaration.name, "call-me");
    assert_eq!(
        span(&code.source, declaration.start, declaration.end),
        "_rdx_63616c6c2d6d65"
    );
    let link = code
        .links
        .iter()
        .find(|link| link.label == "sample.Arithmetic.do-work()V")
        .unwrap();
    assert_eq!(
        span(&code.source, link.start, link.end),
        "_rdx_646f2d776f726b"
    );
}

#[test]
fn reserved_alias_prefix_is_encoded_while_valid_names_stay_visible() {
    let mut class = synthetic(&[0x000e]);
    class.methods[0].name = Arc::from("_rdx_name");
    class.methods[0].return_type = Arc::from("V");
    class.methods.push(DexMethod {
        declaring_type: Arc::from("Lsample/Arithmetic;"),
        name: Arc::from("ordinary"),
        return_type: Arc::from("V"),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 0,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x000e],
            offset: 0,
        }),
    });
    let code = native_java::render("sample.Arithmetic", &class).unwrap();
    assert!(code.source.contains("void _rdx_5f7264785f6e616d65()"));
    assert!(code.source.contains("void ordinary()"));
}

#[test]
fn declared_exception_types_preserve_their_metadata_and_navigation_spans() {
    let mut class = synthetic(&[0x0012, 0x000f]);
    class.methods[0].thrown_types = vec![
        Arc::from("Ljava/io/IOException;"),
        Arc::from("Ljava/lang/ReflectiveOperationException;"),
    ];
    let code = native_java::render_method("sample.Arithmetic", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("throws java.io.IOException, java.lang.ReflectiveOperationException")
    );
    for name in [
        "java.io.IOException",
        "java.lang.ReflectiveOperationException",
    ] {
        let link = code.links.iter().find(|l| l.label == name).unwrap();
        assert_eq!(span(&code.source, link.start, link.end), name);
    }
    for unsupported in [
        "[I",
        "Ljava/lang/String;",
        "Ljava/lang/Object;",
        "Lunknown/Exception;",
    ] {
        class.methods[0].thrown_types = vec![Arc::from(unsupported)];
        assert!(
            native_java::render_method("sample.Arithmetic", &class, &class.methods[0]).is_err()
        );
    }
}
