//! Java 25 flexible constructor body regressions, evaluated from DEX in Rust.
//! Specification background:
//! <https://docs.oracle.com/en/java/javase/25/language/flexible-constructor-bodies.html>
use rdx::{
    native_dex::{self, DexClass, DexField, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn constructor(
    words: Vec<u16>,
    parameters: &[&str],
    symbols: DexSymbols,
    fields: Vec<DexField>,
) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.superclass = Some("Ljava/lang/Object;".into());
    class.fields = fields;
    class.symbols = Arc::new(symbols);
    let method = &mut class.methods[0];
    method.name = "<init>".into();
    method.access_flags = 1;
    method.parameters = parameters.iter().copied().map(Arc::from).collect();
    method.return_type = "V".into();
    method.thrown_types.clear();
    let code = method.code.as_mut().unwrap();
    code.registers = parameters.len() as u16 + 1;
    code.ins = parameters.len() as u16 + 1;
    code.outs = 1;
    code.instructions = words;
    code.tries = 0;
    code.try_regions.clear();
    class
}

fn own_field(name: &str, ty: &str) -> DexField {
    DexField {
        declaring_type: "Lsample/Hello;".into(),
        name: Arc::from(name),
        field_type: Arc::from(ty),
        access_flags: 1,
        is_static: false,
    }
}

fn span(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}

#[test]
fn own_field_write_precedes_super_callback_and_keeps_exact_links() {
    let class = constructor(
        vec![0x0159, 0, 0x1070, 0, 0, 0x000e],
        &["I"],
        DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec![
                "Lsample/Hello;".into(),
                "I".into(),
                "Ljava/lang/Object;".into(),
            ],
            protos: vec![("V".into(), vec![])],
            fields: vec![(0, 1, 0)],
            methods: vec![(2, 0, 1)],
            ..Default::default()
        },
        vec![own_field("value", "I")],
    );

    // Independent DEX evaluator: Object.<init> represents a superclass
    // callback that observes the subclass field at invocation time.
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    let mut field = 0;
    let mut observed = None;
    let mut pc = 0;
    while pc < words.len() {
        match words[pc] as u8 {
            0x59 => {
                assert_eq!(words[pc] >> 8, 1); // value=p0 (r1), receiver=this (r0)
                field = 37;
                pc += 2;
            }
            0x70 => {
                observed = Some(field);
                pc += 3;
            }
            0x0e => break,
            op => panic!("unexpected evaluator opcode {op:#x}"),
        }
    }
    assert_eq!(observed, Some(37));

    let rendered = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    let write = rendered.source.find("this.value = p0;").unwrap();
    let delegation = rendered.source.find("super();").unwrap();
    assert!(write < delegation, "{}", rendered.source);
    let field_link = rendered
        .links
        .iter()
        .find(|link| link.label == "sample.Hello.value:I")
        .unwrap();
    assert_eq!(
        span(&rendered.source, field_link.start, field_link.end),
        "value"
    );
    let ctor_link = rendered
        .links
        .iter()
        .find(|link| link.label == "java.lang.Object.<init>()V")
        .unwrap();
    assert_eq!(
        span(&rendered.source, ctor_link.start, ctor_link.end),
        "super"
    );
}

#[test]
fn inherited_write_own_read_and_this_escape_remain_rejected() {
    let inherited = constructor(
        vec![0x0159, 0, 0x1070, 0, 0, 0x000e],
        &["I"],
        DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec!["Ljava/lang/Object;".into(), "I".into()],
            protos: vec![("V".into(), vec![])],
            fields: vec![(0, 1, 0)],
            methods: vec![(0, 0, 1)],
            ..Default::default()
        },
        vec![],
    );
    assert!(native_java::render_method("sample.Hello", &inherited, &inherited.methods[0]).is_err());

    let read = constructor(
        vec![0x0152, 0, 0x1070, 0, 0, 0x000e],
        &["I"],
        DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec![
                "Lsample/Hello;".into(),
                "I".into(),
                "Ljava/lang/Object;".into(),
            ],
            protos: vec![("V".into(), vec![])],
            fields: vec![(0, 1, 0)],
            methods: vec![(2, 0, 1)],
            ..Default::default()
        },
        vec![own_field("value", "I")],
    );
    assert!(native_java::render_method("sample.Hello", &read, &read.methods[0]).is_err());

    let escape = constructor(
        vec![0x005b, 0, 0x1070, 0, 0, 0x000e],
        &[],
        DexSymbols {
            strings: vec!["peer".into(), "<init>".into()],
            types: vec!["Lsample/Hello;".into(), "Ljava/lang/Object;".into()],
            protos: vec![("V".into(), vec![])],
            fields: vec![(0, 1, 0)],
            methods: vec![(1, 0, 1)],
            ..Default::default()
        },
        vec![own_field("peer", "Ljava/lang/Object;")],
    );
    assert!(native_java::render_method("sample.Hello", &escape, &escape.methods[0]).is_err());
}

fn helper_constructor(argument_register: u16, helper_type: &str) -> DexClass {
    constructor(
        vec![0x1071, 0, argument_register, 0x1070, 1, 0, 0x000e],
        &["I"],
        DexSymbols {
            strings: vec!["check".into(), "<init>".into()],
            types: vec!["Lsample/Hello;".into(), "Ljava/lang/Object;".into()],
            protos: vec![
                ("V".into(), vec![Arc::from(helper_type)]),
                ("V".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1)],
            ..Default::default()
        },
        vec![],
    )
}

#[test]
fn static_helper_parameter_is_preserved_but_this_argument_is_rejected() {
    let safe = helper_constructor(1, "I");
    let rendered = native_java::render_method("sample.Hello", &safe, &safe.methods[0]).unwrap();
    let helper = rendered.source.find("sample.Hello.check(p0);").unwrap();
    let delegation = rendered.source.find("super();").unwrap();
    assert!(helper < delegation, "{}", rendered.source);
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "sample.Hello.check(I)V")
    );
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "java.lang.Object.<init>()V")
    );

    let escaping = helper_constructor(0, "Ljava/lang/Object;");
    assert!(native_java::render_method("sample.Hello", &escaping, &escaping.methods[0]).is_err());
}

#[test]
fn early_field_write_before_same_class_delegation_is_rejected() {
    let class = constructor(
        vec![0x0159, 0, 0x1070, 0, 0, 0x000e],
        &["I"],
        DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec!["Lsample/Hello;".into(), "I".into()],
            protos: vec![("V".into(), vec![])],
            fields: vec![(0, 1, 0)],
            methods: vec![(0, 0, 1)],
            ..Default::default()
        },
        vec![own_field("value", "I")],
    );
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}
