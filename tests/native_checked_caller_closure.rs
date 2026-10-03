use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Constructor,
    Static,
    Public,
}

#[derive(Clone, Copy)]
enum Caller {
    Uncovered,
    Declared,
    Caught,
}

fn raw_fixture(kind: Kind, caller_kind: Caller) -> (DexClass, DexClass) {
    let target_name = if matches!(kind, Kind::Constructor) {
        "<init>"
    } else {
        "emit"
    };
    let target = DexClass {
        descriptor: "Lsample/Target;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: if matches!(kind, Kind::Constructor) {
            1
        } else {
            0x11
        },
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into()],
            types: vec!["Ljava/lang/Object;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Target;".into(),
            name: target_name.into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/io/IOException;".into()],
            thrown_types: vec![],
            access_flags: if matches!(kind, Kind::Static) { 9 } else { 1 },
            code: Some(DexCode {
                registers: if matches!(kind, Kind::Static) { 1 } else { 2 },
                ins: if matches!(kind, Kind::Static) { 1 } else { 2 },
                outs: if matches!(kind, Kind::Constructor) {
                    1
                } else {
                    0
                },
                tries: 0,
                try_regions: vec![],
                instructions: if matches!(kind, Kind::Constructor) {
                    vec![0x1070, 0, 0, 0x0127]
                } else if matches!(kind, Kind::Static) {
                    vec![0x0027]
                } else {
                    vec![0x0127]
                },
                offset: 0,
            }),
        }],
    };
    let caught = matches!(caller_kind, Caller::Caught);
    let (parameters, registers, ins, outs, mut instructions, call_pc) = match kind {
        Kind::Constructor => (
            vec!["Ljava/io/IOException;".into()],
            2,
            1,
            2,
            vec![0x0022, 0, 0x2070, 0, 0x0010, 0x000e],
            2,
        ),
        Kind::Static => (
            vec!["Ljava/io/IOException;".into()],
            1,
            1,
            1,
            vec![0x1071, 0, 0, 0x000e],
            0,
        ),
        Kind::Public => (
            vec!["Lsample/Target;".into(), "Ljava/io/IOException;".into()],
            2,
            2,
            2,
            vec![0x206e, 0, 0x0010, 0x000e],
            0,
        ),
    };
    let try_regions = if caught {
        let handler_pc = instructions.len() as u32;
        instructions.extend([0x000d, 0x000e]);
        vec![DexTryRegion {
            start: call_pc,
            end: call_pc + 3,
            catches: Arc::from([(Some(Arc::from("Ljava/io/IOException;")), handler_pc)]),
        }]
    } else {
        vec![]
    };
    let caller = DexClass {
        descriptor: "Lsample/Caller;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![target_name.into()],
            types: vec!["Lsample/Target;".into()],
            protos: vec![("V".into(), vec!["Ljava/io/IOException;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "call".into(),
            return_type: "V".into(),
            parameters,
            thrown_types: if matches!(caller_kind, Caller::Declared) {
                vec!["Ljava/io/IOException;".into()]
            } else {
                vec![]
            },
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins,
                outs,
                tries: u16::from(caught),
                try_regions,
                instructions,
                offset: 0,
            }),
        }],
    };
    (target, caller)
}

fn with_hierarchy(target: DexClass, caller: DexClass) -> (DexClass, DexClass) {
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&target, &caller]).unwrap());
    target.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    caller.symbols.hierarchy.set(hierarchy).unwrap();
    (target, caller)
}

fn fixture(kind: Kind, caller_kind: Caller) -> (DexClass, DexClass) {
    let (target, caller) = raw_fixture(kind, caller_kind);
    with_hierarchy(target, caller)
}

fn static_alias_fixture(
    caller_kind: Caller,
    shadow: Option<u32>,
    unknown_parent: bool,
    duplicate_child: bool,
) -> DexClass {
    let (mut target, mut caller) = raw_fixture(Kind::Static, caller_kind);
    target.access_flags = 0x401; // Static declaration in an abstract, nonfinal owner.
    Arc::get_mut(&mut caller.symbols).unwrap().types[0] = "Lsample/Child;".into();
    let shadow_method = DexMethod {
        declaring_type: "Lsample/Child;".into(),
        name: "emit".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/io/IOException;".into()],
        thrown_types: vec![],
        access_flags: shadow.unwrap_or(9),
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x000e],
            offset: 0,
        }),
    };
    let child = DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some(if unknown_parent {
            "Lsample/Missing;".into()
        } else {
            "Lsample/Target;".into()
        }),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: if shadow.is_some() {
            vec![shadow_method]
        } else {
            vec![]
        },
    };
    let duplicate = DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: vec![],
    };
    let mut classes = vec![&target, &caller, &child];
    if duplicate_child {
        classes.push(&duplicate);
    }
    let hierarchy = Arc::new(TypeHierarchy::from_classes(classes).unwrap());
    target.symbols.hierarchy.set(hierarchy).unwrap();
    target
}

#[test]
fn inherited_static_alias_requires_complete_caller_proof() {
    let target = static_alias_fixture(Caller::Uncovered, None, false, false);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());

    for caller in [Caller::Declared, Caller::Caught] {
        let target = static_alias_fixture(caller, None, false, false);
        let source = native_java::render_method("sample.Target", &target, &target.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("throws java.io.IOException"), "{source}");
    }
}

#[test]
fn hidden_static_alias_is_not_charged_to_parent_but_malformed_clash_is_rejected() {
    let target = static_alias_fixture(Caller::Uncovered, Some(9), false, false);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_ok());

    let target = static_alias_fixture(Caller::Uncovered, Some(1), false, false);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());
}

#[test]
fn unresolved_static_alias_ancestry_cannot_authorize_new_throws() {
    let target = static_alias_fixture(Caller::Declared, None, true, false);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());

    let target = static_alias_fixture(Caller::Declared, None, false, true);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());
}

#[test]
fn uncovered_exact_callers_block_new_constructor_static_and_public_throws() {
    for kind in [Kind::Constructor, Kind::Static, Kind::Public] {
        let (target, _) = fixture(kind, Caller::Uncovered);
        let error = native_java::render_method("sample.Target", &target, &target.methods[0])
            .unwrap_err()
            .to_string();
        assert!(error.contains("Throw requires"), "{kind:?}: {error}");
    }
}

#[test]
fn declared_and_caught_exact_callers_permit_matching_checked_throws() {
    for kind in [Kind::Constructor, Kind::Static, Kind::Public] {
        for caller in [Caller::Declared, Caller::Caught] {
            let (target, _) = fixture(kind, caller);
            let source = native_java::render_method("sample.Target", &target, &target.methods[0])
                .unwrap()
                .source;
            assert!(
                source.contains("throws java.io.IOException"),
                "{kind:?}: {source}"
            );
        }
    }
}

#[test]
fn constructor_requires_hierarchy_and_nonfinal_public_alias_is_rejected() {
    let (mut target, _) = fixture(Kind::Public, Caller::Declared);
    target.access_flags = 1;
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());

    let (target, _) = fixture(Kind::Constructor, Caller::Declared);
    let bare = DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into()],
            types: vec!["Ljava/lang/Object;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        ..target
    };
    assert!(native_java::render_method("sample.Target", &bare, &bare.methods[0]).is_err());
}

#[test]
fn dex_handler_around_super_call_does_not_cover_java_constructor_declaration() {
    let (target, mut caller) = raw_fixture(Kind::Constructor, Caller::Caught);
    caller.superclass = Some("Lsample/Target;".into());
    caller.methods[0].name = "<init>".into();
    caller.methods[0].access_flags = 1;
    caller.methods[0].code.as_mut().unwrap().ins = 2;
    caller.methods[0].code.as_mut().unwrap().instructions =
        vec![0x2070, 0, 0x0010, 0x000e, 0x000d, 0x000e];
    caller.methods[0].code.as_mut().unwrap().try_regions[0].start = 0;
    caller.methods[0].code.as_mut().unwrap().try_regions[0].end = 3;
    caller.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        Arc::from([(Some(Arc::from("Ljava/io/IOException;")), 4)]);
    let (target, _) = with_hierarchy(target, caller);
    assert!(native_java::render_method("sample.Target", &target, &target.methods[0]).is_err());

    let (target, mut caller) = raw_fixture(Kind::Constructor, Caller::Caught);
    caller.superclass = Some("Lsample/Target;".into());
    caller.methods[0].name = "<init>".into();
    caller.methods[0].access_flags = 1;
    caller.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    caller.methods[0].code.as_mut().unwrap().ins = 2;
    caller.methods[0].code.as_mut().unwrap().instructions =
        vec![0x2070, 0, 0x0010, 0x000e, 0x000d, 0x000e];
    caller.methods[0].code.as_mut().unwrap().try_regions[0].start = 0;
    caller.methods[0].code.as_mut().unwrap().try_regions[0].end = 3;
    caller.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        Arc::from([(Some(Arc::from("Ljava/io/IOException;")), 4)]);
    let (target, _) = with_hierarchy(target, caller);
    let source = native_java::render_method("sample.Target", &target, &target.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"), "{source}");
}

#[test]
fn catchall_callers_cover_checked_exceptions_without_a_typed_entry() {
    for kind in [Kind::Constructor, Kind::Static, Kind::Public] {
        let (target, mut caller) = raw_fixture(kind, Caller::Caught);
        let region = &mut caller.methods[0].code.as_mut().unwrap().try_regions[0];
        region.catches = vec![(None, region.catches[0].1)].into();
        let (target, caller) = with_hierarchy(target, caller);
        let source = native_java::render_method("sample.Target", &target, &target.methods[0])
            .unwrap()
            .source;
        assert!(
            source.contains("throws java.io.IOException"),
            "{kind:?}: {source}"
        );
        // The constructor fixture deliberately allocates before the protected
        // invoke; test its caller proof above without requiring that separate
        // allocation reconstruction shape here.
        if !matches!(kind, Kind::Constructor) {
            let caller_source =
                native_java::render_method("sample.Caller", &caller, &caller.methods[0])
                    .unwrap()
                    .source;
            assert!(
                caller_source.contains("catch (java.lang.Throwable"),
                "{caller_source}"
            );
        }
    }
}
