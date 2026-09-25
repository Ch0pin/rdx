//! Removed no-arg constructors whose implicit Java constructor calls the DEX owner.
use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn empty_class(descriptor: &str, parent: Option<&str>, flags: u32) -> DexClass {
    DexClass {
        descriptor: descriptor.into(),
        superclass: parent.map(Into::into),
        interfaces: vec![],
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn parent(descriptor: &str, constructor_flags: u32) -> DexClass {
    let mut class = empty_class(descriptor, Some("Ljava/lang/Object;"), 1);
    class.methods.push(DexMethod {
        declaring_type: descriptor.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: constructor_flags | 0x10000,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x1070, 0, 0, 0x000e],
            offset: 0,
        }),
    });
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Object;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class
}
fn caller(child: &DexClass, parent: &DexClass) -> DexClass {
    let mut class = empty_class("Lsample/Caller;", Some("Ljava/lang/Object;"), 1);
    class.symbols = Arc::new(DexSymbols {
        types: vec![child.descriptor.clone(), parent.descriptor.clone()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "make".into(),
        return_type: child.descriptor.clone(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x0022, 0, 0x1070, 0, 0, 0x0011],
            offset: 0,
        }),
    });
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, child, parent]).unwrap(),
        ))
        .unwrap();
    class
}

#[test]
fn absent_constructor_uses_implicit_direct_super_call_and_raw_navigation() {
    for access in [0, 1, 4] {
        let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
        let parent = parent("Lsample/Base;", access);
        let class = caller(&child, &parent);
        let code = native_java::render_method("sample.Caller", &class, &class.methods[0]).unwrap();
        assert!(
            code.source.contains("new sample.Child()"),
            "{}",
            code.source
        );
        assert!(!code.source.contains("new sample.Base()"));
        assert!(
            code.links
                .iter()
                .any(|link| link.label == "sample.Base.<init>()V")
        );
    }
}

#[test]
fn implicit_constructor_proof_rejects_inaccessible_or_missing_super_constructor() {
    for access in [0, 2] {
        let child = empty_class("Lother/Child;", Some("Lsample/Base;"), 1);
        let parent = parent("Lsample/Base;", access);
        let class = caller(&child, &parent);
        assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
    }
    let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    let parent = empty_class("Lsample/Base;", Some("Ljava/lang/Object;"), 1);
    let class = caller(&child, &parent);
    // Both loaded classes have no fields, initializers, or constructors. Java's
    // implicit Child() -> Base() -> Object() chain is exactly the observed call.
    let source = native_java::render_method("sample.Caller", &class, &class.methods[0]).unwrap();
    assert!(
        source.source.contains("new sample.Child()"),
        "{}",
        source.source
    );
    assert!(
        native_java::render("sample.Base", &parent)
            .unwrap()
            .source
            .contains("class Base")
    );
    assert!(
        native_java::render("sample.Child", &child)
            .unwrap()
            .source
            .contains("extends Base")
    );
    let missing = empty_class("Lsample/Base;", Some("Lmissing/Ancestor;"), 1);
    let class = caller(&child, &missing);
    assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
}

#[test]
fn implicit_constructor_proof_rejects_nonconcrete_nested_and_existing_constructors() {
    let parent = parent("Lsample/Base;", 1);
    for (descriptor, flags) in [
        ("Lsample/Child;", 0x401),
        ("Lsample/Child;", 0x201),
        ("Lsample/Outer$Child;", 1),
        ("Lsample/Child;", 0x4001),
    ] {
        let child = empty_class(descriptor, Some("Lsample/Base;"), flags);
        assert!(
            !TypeHierarchy::from_classes([&child, &parent])
                .unwrap()
                .equivalent_noarg_constructor(descriptor, "Lsample/Base;")
        );
    }
    let mut child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    child.methods.push(DexMethod {
        declaring_type: child.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["I".into()],
        thrown_types: vec![],
        access_flags: 1,
        code: None,
    });
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
}

#[test]
fn implicit_constructor_proof_rejects_duplicate_parent_and_declared_throws() {
    let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    let mut parent = parent("Lsample/Base;", 1);
    assert!(
        !TypeHierarchy::from_classes([&child, &parent, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
    parent.methods[0]
        .thrown_types
        .push("Ljava/lang/Exception;".into());
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Ljava/lang/Object;")
    );
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn play_store_registration_initializer_reconstructs_implicit_constructor() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata("anfw").unwrap();
    assert!(
        !code.source.contains(".method anfw.<clinit>"),
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("new anfw()").count(),
        1,
        "{}",
        code.source
    );
    assert!(code.source.find("anaq.e()").unwrap() < code.source.find("new anfw()").unwrap());
    assert!(code.source.find("new anfw()").unwrap() < code.source.find(".b(").unwrap());
    assert!(code.source.contains("static {"), "{}", code.source);
    let link = code
        .links
        .iter()
        .find(|link| link.label == "anam.<init>()V")
        .expect("exact superclass constructor identity");
    assert_eq!(
        engine
            .resolve_usage_target("anfw", link.start, &code.source_hash)
            .unwrap()
            .id,
        "anam.<init>()V"
    );
}

#[test]
fn implicit_constructor_can_call_external_object_only_as_direct_parent() {
    let child = empty_class("Lsample/Child;", Some("Ljava/lang/Object;"), 1);
    assert!(
        TypeHierarchy::from_classes([&child])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Ljava/lang/Object;")
    );
    let unrelated = empty_class("Lsample/Other;", Some("Lmissing/Base;"), 1);
    assert!(
        !TypeHierarchy::from_classes([&unrelated])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Other;", "Ljava/lang/Object;")
    );
    let abstract_child = empty_class("Lsample/Abstract;", Some("Ljava/lang/Object;"), 0x401);
    assert!(
        !TypeHierarchy::from_classes([&abstract_child])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Abstract;", "Ljava/lang/Object;")
    );
}

#[test]
fn observed_parameter_constructor_can_cross_proven_transparent_forwarder() {
    let mut base = parent("Lsample/Base;", 1);
    base.methods[0].parameters = vec!["I".into()];
    base.methods[0].code.as_mut().unwrap().registers = 2;
    base.methods[0].code.as_mut().unwrap().ins = 2;
    let mut middle = parent("Lsample/Middle;", 1);
    middle.superclass = Some(base.descriptor.clone());
    middle.methods[0].parameters = vec!["I".into()];
    middle.symbols = Arc::new(DexSymbols {
        types: vec![base.descriptor.clone()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let code = middle.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 2;
    code.outs = 2;
    code.instructions = vec![0x2070, 0, 0x0010, 0x000e];
    let leaf = empty_class("Lsample/Leaf;", Some("Lsample/Middle;"), 1);
    let mut caller = caller(&leaf, &base);
    Arc::get_mut(&mut caller.symbols).unwrap().protos[0].1 = vec!["I".into()];
    let code = caller.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.outs = 2;
    code.instructions = vec![0x0022, 0, 0x7112, 0x2070, 0, 0x0010, 0x0011];
    let hierarchy = TypeHierarchy::from_classes([&leaf, &middle, &base, &caller]).unwrap();
    assert!(hierarchy.equivalent_constructor(&leaf.descriptor, &base.descriptor, &["I".into()]));
    let ctor = &hierarchy.recovered_constructors(&leaf.descriptor)[0];
    assert_eq!(ctor.parent, middle.descriptor);
    assert_eq!(ctor.invoked_owner, base.descriptor);
    // A constant replacing the forwarded argument changes behavior and must fail.
    middle.methods[0].code.as_mut().unwrap().instructions = vec![0x0112, 0x2070, 0, 0x0010, 0x000e];
    let hierarchy = TypeHierarchy::from_classes([&leaf, &middle, &base, &caller]).unwrap();
    assert!(!hierarchy.equivalent_constructor(&leaf.descriptor, &base.descriptor, &["I".into()]));
}
