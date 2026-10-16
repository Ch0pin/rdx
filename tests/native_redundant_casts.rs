//! Production renderer regression: casts may disappear only with complete call evidence.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture(overloaded: bool) -> DexClass {
    let owner: Arc<str> = "Lsample/Target;".into();
    let params: Vec<Arc<str>> = [
        "Ljava/lang/Object;",
        "Ljava/lang/String;",
        "Ljava/lang/String;",
    ]
    .into_iter()
    .map(Arc::from)
    .collect();
    let symbols = Arc::new(DexSymbols {
        strings: ["m", "1", "$setOnce", "BETA"]
            .into_iter()
            .map(String::from)
            .collect(),
        types: vec![owner.clone()],
        protos: vec![("V".into(), params.clone())],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let caller = DexClass {
        symbols,
        descriptor: "Lsample/Caller;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "call".into(),
            return_type: "V".into(),
            parameters: vec![owner.clone()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 4,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x001a, 1, 0x011a, 2, 0x021a, 3, 0x406e, 0, 0x2103, 0x000e],
                offset: 0,
            }),
        }],
    };
    let mut target = DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "m".into(),
            return_type: "V".into(),
            parameters: params,
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        }],
    };
    if overloaded {
        let original = &target.methods[0];
        let mut other = DexMethod {
            declaring_type: original.declaring_type.clone(),
            name: original.name.clone(),
            return_type: original.return_type.clone(),
            parameters: original.parameters.clone(),
            thrown_types: vec![],
            access_flags: original.access_flags,
            code: None,
        };
        other.parameters[0] = "Ljava/lang/String;".into();
        target.methods.push(other);
    }
    caller
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&caller, &target]).unwrap(),
        ))
        .unwrap();
    caller
}

#[test]
fn full_renderer_removes_safe_cast_but_retains_overload_selection() {
    for overloaded in [false, true] {
        let class = fixture(overloaded);
        let raw = native_java::render_method("sample.Caller", &class, &class.methods[0]).unwrap();
        assert!(raw.source.contains("((java.lang.Object)"), "{}", raw.source);
        let rendered = native_java::render("sample.Caller", &class).unwrap();
        assert_eq!(
            rendered.source.contains("((Object)"),
            overloaded,
            "{}",
            rendered.source
        );
        assert!(!rendered.source.contains(".method"), "{}", rendered.source);
        let link = rendered
            .links
            .iter()
            .find(|l| l.label.starts_with("sample.Target.m("))
            .unwrap();
        assert_eq!(
            rendered
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "m"
        );
    }
}
