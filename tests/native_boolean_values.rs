//! Boolean register values remain boolean until a DEX integer use requires an
//! explicit Java 0/1 projection. Arbitrary integers are never guessed boolean.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(words: &[u16], parameters: &[&str], return_type: &str, registers: u16) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = "booleanValues".into();
    method.parameters = parameters.iter().copied().map(Arc::from).collect();
    method.access_flags = 9;
    method.return_type = Arc::from(return_type);
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = parameters.len() as u16;
    code.instructions = words.to_vec();
    class
}

fn render(class: &DexClass) -> anyhow::Result<String> {
    native_java::render_method("sample.Hello", class, &class.methods[0]).map(|code| code.source)
}

#[test]
fn merged_zero_one_constants_project_to_int_for_arithmetic() {
    // v0 = p0 ? 0 : 1; return v0 + 1
    let class = fixture(
        &[0x0138, 4, 0x0012, 0x0228, 0x1012, 0x00d8, 0x0100, 0x000f],
        &["Z"],
        "I",
        2,
    );
    let source = render(&class).unwrap();
    assert!(source.contains("? 1 : 0"), "{source}");
    assert!(source.contains(" + (1)"), "{source}");
}

#[test]
fn boolean_equality_stays_boolean_but_ordering_uses_zero_one_projection() {
    let equality = fixture(
        &[
            0x0138, 4, 0x0012, 0x0228, 0x1012, 0x0038, 4, 0x0012, 0x000f, 0x1012, 0x000f,
        ],
        &["Z"],
        "I",
        2,
    );
    let equality = render(&equality).unwrap();
    assert!(equality.contains("false"), "{equality}");
    assert!(!equality.contains("? 1 : 0"), "{equality}");

    let ordered = fixture(
        &[
            0x0138, 4, 0x0012, 0x0228, 0x1012, 0x003a, 4, 0x0012, 0x000f, 0x1012, 0x000f,
        ],
        &["Z"],
        "I",
        2,
    );
    let ordered = render(&ordered).unwrap();
    assert!(ordered.contains("? 1 : 0"), "{ordered}");
    assert!(
        ordered.contains(" < 0") || ordered.contains(" >= 0"),
        "{ordered}"
    );
}

fn invocation_fixture(parameter: &str) -> DexClass {
    let mut class = fixture(
        &[
            0x0138, 4, 0x0012, 0x0228, 0x1012, 0x1071, 0, 0, 0x1071, 1, 0, 0x000e,
        ],
        &[parameter],
        "V",
        2,
    );
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["takeInt".into(), "takeBool".into()],
        types: vec!["Lsample/Hello;".into()],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("V".into(), vec!["Z".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    class
}

#[test]
fn typed_invocations_keep_int_and_boolean_overloads_distinct() {
    let source = render(&invocation_fixture("Z")).unwrap();
    assert!(
        source.contains("takeInt") && source.contains("? 1 : 0"),
        "{source}"
    );
    assert!(source.contains("takeBool"), "{source}");

    // An arbitrary int passed directly may not be narrowed merely because the
    // consumer has a boolean descriptor.
    let mut arbitrary = invocation_fixture("I");
    arbitrary.methods[0].code.as_mut().unwrap().registers = 1;
    arbitrary.methods[0].code.as_mut().unwrap().instructions = vec![0x1071, 1, 0, 0x000e];
    assert!(render(&arbitrary).is_err());
}

fn boolean_failure_fixture(literal: i8) -> DexClass {
    let mut class = fixture(
        &[
            0x0071,
            0,
            0,
            0x080a,
            0x08df,
            ((literal as u8 as u16) << 8) | 8,
            0x080f,
        ],
        &[],
        "Z",
        9,
    );
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["userMethod".into()],
        types: vec!["Lsample/Hello;".into()],
        protos: vec![("Z".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class
}

#[test]
fn boolean_invoke_result_xor_true_remains_boolean() {
    // Real compiler pattern: invoke returning Z; move-result; xor-int/lit8 v0, v0, #1; return v0.
    let source = render(&boolean_failure_fixture(1)).unwrap();
    assert!(
        source.contains("boolean v0 = sample.Hello.userMethod();"),
        "{source}"
    );
    assert!(source.contains("boolean v1 = !(v0);"), "{source}");
    assert!(source.contains("return v1;"), "{source}");

    let lowered = |user_method: bool| !user_method;
    assert!(!lowered(true));
    assert!(lowered(false));
}

#[test]
fn xor_literal_keeps_boolean_and_integer_semantics_distinct() {
    let boolean_identity = render(&boolean_failure_fixture(0)).unwrap();
    assert!(
        boolean_identity.contains("boolean v1 = v0;"),
        "{boolean_identity}"
    );

    let integer = fixture(&[0x00df, 0x0100, 0x000f], &["I"], "I", 1);
    let integer = render(&integer).unwrap();
    assert!(integer.contains("int v0 = p0 ^ (1);"), "{integer}");
    assert!(!integer.contains("boolean v0"), "{integer}");
}
