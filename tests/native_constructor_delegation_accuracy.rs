//! Static constructor arguments preserve evaluation order and original symbols.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(words: Vec<u16>, arguments: &[&str]) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.fields.clear();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["a".into(), "b".into(), "<init>".into()],
        types: vec![
            "Lsample/Hello;".into(),
            "Lsample/Source;".into(),
            "Lsample/Sub;".into(),
        ],
        fields: vec![(1, 2, 0), (1, 2, 1)],
        protos: vec![(
            "V".into(),
            arguments.iter().copied().map(Arc::from).collect(),
        )],
        methods: vec![(0, 0, 2)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = "<init>".into();
    m.access_flags = 1;
    m.parameters = vec!["[B".into()];
    m.return_type = "V".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2; // this r2, unused byte[] r3
    code.outs = 3;
    code.instructions = words;
    code.tries = 0;
    code.try_regions.clear();
    class
}

#[test]
fn independent_static_read_delegates_with_exact_overload_and_links() {
    let class = fixture(
        vec![0x0062, 0, 0x2070, 0, 0x0002, 0x000e],
        &["Lsample/Base;"],
    );
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(!code.source.contains(".method"), "{}", code.source);
    assert!(
        code.source.contains("this(((sample.Base) Source.a));"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("Source.a").count(), 1);
    for (label, token) in [
        ("sample.Source.a:Lsample/Sub;", "a"),
        ("sample.Hello.<init>(Lsample/Base;)V", "this"),
    ] {
        let link = code.links.iter().find(|l| l.label == label).expect(label);
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            token
        );
    }
}

#[test]
fn two_reads_keep_original_order_when_arguments_reverse_them() {
    let class = fixture(
        vec![0x0062, 0, 0x0162, 1, 0x3070, 0, 0x0012, 0x000e],
        &["Lsample/Sub;", "Lsample/Sub;"],
    );
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.find("sample.Source.a").unwrap() < code.source.find("sample.Source.b").unwrap()
    );
    assert_eq!(code.source.matches("sample.Source.a").count(), 1);
    assert_eq!(code.source.matches("sample.Source.b").count(), 1);
    assert!(code.source.contains("this(v1, v0)"), "{}", code.source);
}

#[test]
fn shared_read_is_evaluated_once_and_static_write_remains_rejected() {
    let class = fixture(
        vec![0x0062, 0, 0x3070, 0, 0x0002, 0x000e],
        &["Lsample/Sub;", "Lsample/Sub;"],
    );
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(!code.source.contains(".method"), "{}", code.source);
    assert_eq!(code.source.matches("Source.a").count(), 1);
    let write = fixture(
        vec![0x0069, 0, 0x2070, 0, 0x0002, 0x000e],
        &["Lsample/Base;"],
    );
    assert!(native_java::render_method("sample.Hello", &write, &write.methods[0]).is_err());
}

#[test]
fn throw_before_parent_initialization_does_not_invent_super_effects() {
    let class = fixture(vec![0x0012, 0x0027], &[]);
    let err = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .err()
        .unwrap();
    assert!(
        err.to_string().contains("throw before initialization"),
        "{err:#}"
    );
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn reported_caso_constructor_inventory() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = engine.class("caso").unwrap();
    let mut reconstructed = 0;
    let mut throw_only = 0;
    let mut arrays = 0;
    for method in class.methods.iter().filter(|m| m.name.as_ref() == "<init>") {
        let result = native_java::render_method("caso", class, method);
        if method.parameters.is_empty() {
            let err = result.expect_err("Do not invent super() for throw-only constructor");
            assert!(
                err.to_string().contains("throw before initialization"),
                "{err:#}"
            );
            throw_only += 1;
        } else {
            let code = result.unwrap_or_else(|e| panic!("{:?}: {e:#}", method.parameters));
            assert!(!code.source.contains(".method"));
            reconstructed += 1;
            if method.parameters.iter().any(|p| p.starts_with('[')) {
                arrays += 1;
                assert!(code.source.contains("this("), "{}", code.source);
                assert!(code.links.iter().any(|l| l.label == "caso.<init>(Lcasu;)V"));
            }
        }
    }
    assert_eq!(throw_only, 1);
    assert!(
        arrays > 10,
        "Expected broad reported overload inventory, got {arrays}"
    );
    eprintln!(
        "caso constructors: {reconstructed} reconstructed, {throw_only} precise throw-only fallback, {arrays} array overloads"
    );
}
