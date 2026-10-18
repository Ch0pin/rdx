use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn class(owner: &str) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn fixture(catchall: bool) -> Vec<DexClass> {
    let mut child = class("Lsample/Child;");
    let mut external = class("Lsample/External;");
    child.fields.push(DexField {
        declaring_type: child.descriptor.clone(),
        name: "f".into(),
        field_type: "I".into(),
        access_flags: 17,
        is_static: false,
    });
    external.fields.push(DexField {
        declaring_type: external.descriptor.clone(),
        name: "x".into(),
        field_type: "I".into(),
        access_flags: 1,
        is_static: false,
    });
    child.symbols = Arc::new(DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            child.descriptor.clone(),
            "I".into(),
            external.descriptor.clone(),
        ],
        strings: vec!["<init>".into(), "x".into(), "f".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(3, 2, 1), (1, 2, 2)],
        ..Default::default()
    });
    child.methods.push(DexMethod {
        declaring_type: child.descriptor.clone(),
        name: "<init>".into(),
        parameters: vec!["I".into(), external.descriptor.clone()],
        return_type: "V".into(),
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 4,
            ins: 3,
            outs: 1,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 5,
                end: 7,
                catches: vec![(
                    if catchall {
                        None
                    } else {
                        Some("Ljava/lang/RuntimeException;".into())
                    },
                    8,
                )]
                .into(),
            }],
            instructions: vec![
                0x1070, 0, 1, 0x3259, 0, 0x1259, 1, 0x0328, 0x000d, 0x0027, 0x000e,
            ],
            offset: 0,
        }),
    });
    vec![child, external]
}
fn install(classes: &[DexClass]) -> Arc<TypeHierarchy> {
    let hierarchy = Arc::new(TypeHierarchy::from_classes(classes.iter()).unwrap());
    for class in classes {
        class.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    }
    hierarchy
}
fn render(classes: &[DexClass]) -> anyhow::Result<String> {
    let child = classes
        .iter()
        .find(|c| c.descriptor.as_ref() == "Lsample/Child;")
        .unwrap();
    native_java::render_method("sample.Child", child, &child.methods[0]).map(|c| c.source)
}
#[test]
fn typed_protected_final_body_receives_exact_public_field_authority() {
    for catchall in [false, true] {
        let classes = fixture(catchall);
        install(&classes);
        let source = render(&classes).unwrap();
        assert!(source.contains("p1.x = p0;"), "{source}");
        assert!(source.contains("this.f = p0;"), "{source}");
        assert!(source.contains("throw e"), "{source}");
    }
}
#[test]
fn typed_queries_preserve_declaration_authority_and_terminal_failure_requirements() {
    for mode in 0..9 {
        let mut classes = fixture(false);
        match mode {
            0 => classes[1].fields[0].access_flags = 17,
            1 => classes[1].fields[0].access_flags = 2,
            2 => {
                classes[1].fields[0].is_static = true;
                classes[1].fields[0].access_flags = 9;
            }
            3 => classes[1].fields[0].field_type = "J".into(),
            4 => classes[1].fields[0].declaring_type = "Lsample/Other;".into(),
            5 => {
                let mut duplicate = class("Lsample/External;");
                duplicate.fields.push(DexField {
                    declaring_type: duplicate.descriptor.clone(),
                    name: "x".into(),
                    field_type: "I".into(),
                    access_flags: 1,
                    is_static: false,
                });
                classes.push(duplicate);
            }
            6 => {
                let owner = classes[1].descriptor.clone();
                classes[1].fields.push(DexField {
                    declaring_type: owner,
                    name: "x".into(),
                    field_type: "J".into(),
                    access_flags: 1,
                    is_static: false,
                });
            }
            7 => classes[0].methods[0].code.as_mut().unwrap().instructions[9] = 0x000e,
            8 => classes[0].methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .push(0x0014),
            _ => unreachable!(),
        }
        install(&classes);
        assert!(render(&classes).is_err(), "mode {mode}");
    }
}
#[test]
fn typed_and_catchall_class_order_preserves_source() {
    let first = fixture(false);
    install(&first);
    let expected = render(&first).unwrap();
    let mut second = fixture(false);
    second.reverse();
    install(&second);
    assert_eq!(render(&second).unwrap(), expected);
}
