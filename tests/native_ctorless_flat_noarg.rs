use rdx::{
    native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexAnnotationSet, DexClass, DexCode, DexField,
        DexMethod, DexSymbols, DexTryRegion, DexValue,
    },
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::collections::BTreeMap;
use std::sync::Arc;

const OBJECT: &str = "Ljava/lang/Object;";

fn class(owner: &str, parent: &str, flags: u32) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    }
}

fn parent(owner: &str) -> DexClass {
    let mut parent = class(owner, OBJECT, 0x401);
    parent.symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into()],
        types: vec![OBJECT.into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    parent.methods.push(DexMethod {
        declaring_type: owner.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x1070, 0, 0, 0x000e],
        }),
    });
    parent
}

fn abstract_method(owner: &str, name: &str) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x401,
        code: None,
    }
}

fn concrete_method(owner: &str, name: &str) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 1,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x000e],
        }),
    }
}

fn concrete_signature(owner: &str, name: &str, parameters: &[&str], ret: &str) -> DexMethod {
    let parameter_words = u16::try_from(parameters.len()).unwrap();
    let result_register = parameter_words;
    let instructions = if ret.starts_with('L') || ret.starts_with('[') {
        vec![(result_register << 8) | 0x12, (result_register << 8) | 0x11]
    } else {
        vec![(result_register << 8) | 0x12, (result_register << 8) | 0x0f]
    };
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: parameters.iter().map(|value| (*value).into()).collect(),
        thrown_types: vec![],
        access_flags: 1,
        code: Some(DexCode {
            registers: parameter_words + 2,
            ins: parameter_words + 1,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions,
        }),
    }
}

fn interface(owner: &str, name: &str) -> DexClass {
    let mut interface = class(owner, OBJECT, 0x601);
    interface.methods.push(abstract_method(owner, name));
    interface
}

fn caller(owner: &str, parent: &str, words: Vec<u16>, registers: u16) -> DexClass {
    let mut caller = class("Lsample/Factory;", OBJECT, 1);
    caller.symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into()],
        types: vec![owner.into(), parent.into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    caller.methods.push(DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "make".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    });
    caller
}

fn exact_caller(owner: &str, parent: &str) -> DexClass {
    caller(owner, parent, vec![0x0022, 0, 0x1070, 0, 0, 0x000e], 1)
}

fn annotations(
    _owner: &str,
    enclosing: &str,
    inner_name: Option<&str>,
    flags: i32,
) -> Arc<DexSymbols> {
    let enclosing_descriptor: Arc<str> = enclosing.into();
    let mut strings = vec!["accessFlags".into(), "name".into(), "value".into()];
    let name_index = inner_name.map(|name| {
        let index = u32::try_from(strings.len()).unwrap();
        strings.push(name.into());
        index
    });
    let inner = Arc::new(DexAnnotation {
        visibility: 2,
        type_idx: 0,
        elements: vec![
            (0, DexValue::Int(flags)),
            (1, name_index.map_or(DexValue::Null, DexValue::String)),
        ],
    });
    let enclosing_annotation = Arc::new(DexAnnotation {
        visibility: 2,
        type_idx: 1,
        elements: vec![(2, DexValue::Type(2))],
    });
    let set: DexAnnotationSet = vec![inner, enclosing_annotation].into();
    let mut directories = BTreeMap::new();
    directories.insert(
        1,
        Arc::new(DexAnnotationDirectory {
            class: Some(set),
            ..Default::default()
        }),
    );
    Arc::new(DexSymbols {
        strings,
        types: vec![
            "Ldalvik/annotation/InnerClass;".into(),
            "Ldalvik/annotation/EnclosingClass;".into(),
            enclosing_descriptor,
        ],
        annotations: directories,
        ..Default::default()
    })
}

fn anonymous(owner: &str, enclosing: &str, parent: &str) -> DexClass {
    let mut result = class(owner, parent, 0);
    result.annotations_offset = 1;
    result.symbols = annotations(owner, enclosing, None, 1);
    result
}

fn named_static(owner: &str, enclosing: &str, name: &str) -> DexClass {
    let mut result = class(owner, OBJECT, 0);
    result.annotations_offset = 1;
    result.symbols = annotations(owner, enclosing, Some(name), 9);
    result.fields.extend([
        DexField {
            declaring_type: owner.into(),
            name: "a".into(),
            field_type: "[C".into(),
            access_flags: 1,
            is_static: false,
        },
        DexField {
            declaring_type: owner.into(),
            name: "b".into(),
            field_type: "Ljava/lang/String;".into(),
            access_flags: 1,
            is_static: false,
        },
    ]);
    result
}

fn install(classes: &[&DexClass]) -> Arc<TypeHierarchy> {
    let hierarchy = Arc::new(TypeHierarchy::from_classes(classes.iter().copied()).unwrap());
    for class in classes {
        class.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    }
    hierarchy
}

#[test]
fn independent_flat_class_is_order_independent_and_keeps_package_constructor() {
    for order in 0..4 {
        let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let parent = parent("Lsample/Parent;");
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let classes = match order {
            0 => vec![&leaf, &parent, &caller],
            1 => vec![&caller, &leaf, &parent],
            2 => vec![&parent, &caller, &leaf],
            _ => vec![&caller, &parent, &leaf],
        };
        let hierarchy = install(&classes);
        let [record] = hierarchy.recovered_constructors(&leaf.descriptor) else {
            panic!("missing unique recovered constructor");
        };
        assert_eq!(record.access_flags, 0);
        assert_eq!(record.parent, parent.descriptor);
        assert!(record.parameters.is_empty() && record.thrown_types.is_empty());
        let source = native_java::render("sample.Leaf$4", &leaf).unwrap().source;
        assert!(source.contains("\n    Leaf$4() {"), "{source}");
        assert!(!source.contains("public Leaf$4()"), "{source}");
    }
}

#[test]
fn loaded_parent_static_initializer_does_not_block_child_header_authority() {
    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let mut parent = parent("Lsample/Parent;");
    parent.symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into(), "INSTANCE".into()],
        types: vec![OBJECT.into(), parent.descriptor.clone()],
        protos: vec![("V".into(), vec![])],
        fields: vec![(1, 1, 1)],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    parent.fields.push(DexField {
        declaring_type: parent.descriptor.clone(),
        name: "INSTANCE".into(),
        field_type: parent.descriptor.clone(),
        access_flags: 0x19,
        is_static: true,
    });
    parent.methods.push(DexMethod {
        declaring_type: parent.descriptor.clone(),
        name: "<clinit>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10008,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0012, 0x0069, 0, 0x000e],
        }),
    });
    let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
    let hierarchy = install(&[&leaf, &parent, &caller]);
    assert_eq!(hierarchy.recovered_constructors(&leaf.descriptor).len(), 1);
}

#[test]
fn exact_anonymous_and_named_static_metadata_remain_flat_constructor_candidates() {
    let anonymous = anonymous("Lsample/Outer$1;", "Lsample/Outer;", OBJECT);
    let mut anonymous_caller = exact_caller(&anonymous.descriptor, OBJECT);
    anonymous_caller.descriptor = "Lsample/AnonymousFactory;".into();
    anonymous_caller.methods[0].declaring_type = anonymous_caller.descriptor.clone();
    let current = named_static(
        "Lsample/Outer$CurrentWrite;",
        "Lsample/Outer;",
        "CurrentWrite",
    );
    let mut current = current;
    current.interfaces.push("Ljava/lang/CharSequence;".into());
    let current_owner = current.descriptor.clone();
    current.methods.extend([
        concrete_signature(&current_owner, "charAt", &["I"], "C"),
        concrete_signature(&current_owner, "length", &[], "I"),
        concrete_signature(
            &current_owner,
            "subSequence",
            &["I", "I"],
            "Ljava/lang/CharSequence;",
        ),
        concrete_signature(&current_owner, "toString", &[], "Ljava/lang/String;"),
    ]);
    let mut current_caller = exact_caller(&current.descriptor, OBJECT);
    current_caller.descriptor = "Lsample/CurrentFactory;".into();
    current_caller.methods[0].declaring_type = current_caller.descriptor.clone();
    let hierarchy = install(&[&anonymous, &anonymous_caller, &current, &current_caller]);
    for owner in [&anonymous.descriptor, &current.descriptor] {
        let [record] = hierarchy.recovered_constructors(owner) else {
            panic!("missing flat recovered constructor for {owner}");
        };
        assert_eq!(record.access_flags, 0);
        assert_eq!(record.parent.as_ref(), OBJECT);
    }
}

#[test]
fn generic_signature_and_capture_state_decline() {
    let mut generic = class("Lsample/Generic$4;", "Lsample/Parent;", 0);
    generic.annotations_offset = 1;
    let signature = Arc::new(DexAnnotation {
        visibility: 2,
        type_idx: 0,
        elements: vec![(0, DexValue::Array(vec![DexValue::String(1)]))],
    });
    let mut directories = BTreeMap::new();
    directories.insert(
        1,
        Arc::new(DexAnnotationDirectory {
            class: Some(vec![signature].into()),
            ..Default::default()
        }),
    );
    generic.symbols = Arc::new(DexSymbols {
        strings: vec!["value".into(), "Ljava/lang/Object;".into()],
        types: vec!["Ldalvik/annotation/Signature;".into()],
        annotations: directories,
        ..Default::default()
    });
    let parent = parent("Lsample/Parent;");
    let mut generic_caller = exact_caller(&generic.descriptor, &parent.descriptor);
    generic_caller.descriptor = "Lsample/GenericFactory;".into();
    generic_caller.methods[0].declaring_type = generic_caller.descriptor.clone();

    let mut capture = anonymous("Lsample/Outer$1;", "Lsample/Outer;", OBJECT);
    capture.fields.push(DexField {
        declaring_type: capture.descriptor.clone(),
        name: "captured".into(),
        field_type: OBJECT.into(),
        access_flags: 1,
        is_static: false,
    });
    let mut capture_caller = exact_caller(&capture.descriptor, OBJECT);
    capture_caller.descriptor = "Lsample/CaptureFactory;".into();
    capture_caller.methods[0].declaring_type = capture_caller.descriptor.clone();
    let hierarchy = install(&[
        &generic,
        &parent,
        &generic_caller,
        &capture,
        &capture_caller,
    ]);
    assert!(
        hierarchy
            .recovered_constructors(&generic.descriptor)
            .is_empty()
    );
    assert!(
        hierarchy
            .recovered_constructors(&capture.descriptor)
            .is_empty()
    );
}

#[test]
fn retained_constructor_final_field_and_class_initializer_decline() {
    for mode in 0..3 {
        let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        match mode {
            0 => leaf.methods.push(DexMethod {
                declaring_type: leaf.descriptor.clone(),
                name: "<init>".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 0x10000,
                code: None,
            }),
            1 => leaf.fields.push(DexField {
                declaring_type: leaf.descriptor.clone(),
                name: "value".into(),
                field_type: OBJECT.into(),
                access_flags: 0x10,
                is_static: false,
            }),
            2 => leaf.methods.push(DexMethod {
                declaring_type: leaf.descriptor.clone(),
                name: "<clinit>".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 0x10008,
                code: None,
            }),
            _ => unreachable!(),
        }
        let parent = parent("Lsample/Parent;");
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&leaf, &parent, &caller]);
        assert!(
            hierarchy
                .recovered_constructors(&leaf.descriptor)
                .is_empty()
        );
    }
}

#[test]
fn intervening_use_and_branch_bypass_decline() {
    for (words, registers) in [
        (vec![0x0022, 0, 0x0107, 0x1070, 0, 1, 0x000e], 2),
        (vec![0x0022, 0, 0x0029, 5, 0x1070, 0, 0, 0x000e], 1),
    ] {
        let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let parent = parent("Lsample/Parent;");
        let caller = caller(&leaf.descriptor, &parent.descriptor, words, registers);
        let hierarchy = install(&[&leaf, &parent, &caller]);
        assert!(
            hierarchy
                .recovered_constructors(&leaf.descriptor)
                .is_empty()
        );
    }
}

#[test]
fn protected_pair_and_constructor_argument_decline() {
    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let parent_class = parent("Lsample/Parent;");
    let mut protected = caller(
        &leaf.descriptor,
        &parent_class.descriptor,
        vec![0x0022, 0, 0x1070, 0, 0, 0x0328, 0x000d, 0x000e, 0x000e],
        1,
    );
    let protected_code = protected.methods[0].code.as_mut().unwrap();
    protected_code.tries = 1;
    protected_code.try_regions.push(DexTryRegion {
        start: 0,
        end: 5,
        catches: vec![(None, 6)].into(),
    });
    let hierarchy = install(&[&leaf, &parent_class, &protected]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );

    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let parent_class2 = parent("Lsample/Parent;");
    let mut argument = caller(
        &leaf.descriptor,
        &parent_class2.descriptor,
        vec![0x0112, 0x0022, 0, 0x2070, 0, 0x0010, 0x000e],
        2,
    );
    let symbols = Arc::get_mut(&mut argument.symbols).unwrap();
    symbols.protos[0].1.push(OBJECT.into());
    argument.methods[0].code.as_mut().unwrap().outs = 2;
    let hierarchy = install(&[&leaf, &parent_class2, &argument]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );
}

#[test]
fn wrong_final_effectful_or_nonpublic_parent_declines() {
    for mode in 0..6 {
        let parent_name = "Lsample/Parent;";
        let leaf = class("Lsample/Leaf$4;", parent_name, 0);
        let mut parent = parent(parent_name);
        match mode {
            0 => parent.access_flags |= 0x10,
            1 => parent.methods[0]
                .thrown_types
                .push("Ljava/lang/Exception;".into()),
            2 => {
                let symbols = Arc::get_mut(&mut parent.symbols).unwrap();
                symbols.types.push("Lsample/Hooks;".into());
                symbols.strings.push("effect".into());
                symbols.methods.push((1, 0, 1));
                parent.methods[0].code.as_mut().unwrap().instructions =
                    vec![0x1070, 0, 0, 0x0071, 1, 0, 0x000e];
            }
            3 => parent.access_flags = 0x400,
            4 => parent.methods[0].access_flags = 0x10004,
            5 => parent.methods[0].access_flags = 0x10002,
            _ => unreachable!(),
        }
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&leaf, &parent, &caller]);
        assert!(
            hierarchy
                .recovered_constructors(&leaf.descriptor)
                .is_empty()
        );
    }
}

#[test]
fn abstract_parent_and_interface_obligations_require_exact_public_bodies() {
    for from_interface in [false, true] {
        {
            let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
            let mut parent = parent("Lsample/Parent;");
            let contract = interface("Lsample/Contract;", "run");
            let parent_owner = parent.descriptor.clone();
            if from_interface {
                leaf.interfaces.push(contract.descriptor.clone());
            } else {
                parent.methods.push(abstract_method(&parent_owner, "run"));
            }
            let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
            let hierarchy = install(&[&leaf, &parent, &contract, &caller]);
            assert!(
                hierarchy
                    .recovered_constructors(&leaf.descriptor)
                    .is_empty()
            );
        }

        let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let mut parent = parent("Lsample/Parent;");
        let contract = interface("Lsample/Contract;", "run");
        let leaf_owner = leaf.descriptor.clone();
        let parent_owner = parent.descriptor.clone();
        if from_interface {
            leaf.interfaces.push(contract.descriptor.clone());
        } else {
            parent.methods.push(abstract_method(&parent_owner, "run"));
        }
        leaf.methods.push(concrete_method(&leaf_owner, "run"));
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&caller, &contract, &parent, &leaf]);
        assert_eq!(hierarchy.recovered_constructors(&leaf.descriptor).len(), 1);
    }
}

#[test]
fn malformed_implementation_and_unknown_inherited_interface_decline() {
    for mode in 0..16 {
        let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let parent = parent("Lsample/Parent;");
        let mut contract = interface("Lsample/Contract;", "run");
        let leaf_owner = leaf.descriptor.clone();
        let contract_owner = contract.descriptor.clone();
        leaf.interfaces.push(contract.descriptor.clone());
        let mut implementation = concrete_method(&leaf.descriptor, "run");
        match mode {
            0 => implementation.return_type = "I".into(),
            1 => implementation.access_flags = 9,
            2 => implementation.access_flags = 0,
            3 => implementation.code = None,
            4 => leaf.methods.push(concrete_method(&leaf_owner, "run")),
            5 => contract.interfaces.push("Lmissing/Inherited;".into()),
            6 => contract.methods[0].access_flags = 0x409,
            7 => contract.methods[0].access_flags = 0x400,
            8 => contract.methods[0].return_type = "I".into(),
            9 => contract
                .methods
                .push(abstract_method(&contract_owner, "run")),
            10 => contract.superclass = Some("Lsample/Other;".into()),
            11 => contract.access_flags = 0x201,
            12 => contract.access_flags = 0x604,
            13 => leaf.interfaces.push(contract.descriptor.clone()),
            14 => {
                let mut method = concrete_method(&contract_owner, "helper");
                method.access_flags = 9;
                contract.methods.push(method);
            }
            15 => {
                let mut method = concrete_method(&contract_owner, "<clinit>");
                method.access_flags = 8;
                contract.methods.push(method);
            }
            _ => unreachable!(),
        }
        leaf.methods.push(implementation);
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&leaf, &parent, &contract, &caller]);
        assert!(
            hierarchy
                .recovered_constructors(&leaf.descriptor)
                .is_empty()
        );
    }
}

#[test]
fn duplicate_parent_abstract_declaration_declines() {
    let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let mut parent = parent("Lsample/Parent;");
    let leaf_owner = leaf.descriptor.clone();
    let parent_owner = parent.descriptor.clone();
    leaf.methods.push(concrete_method(&leaf_owner, "run"));
    parent.methods.push(abstract_method(&parent_owner, "run"));
    parent.methods.push(abstract_method(&parent_owner, "run"));
    let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
    let hierarchy = install(&[&leaf, &parent, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );
}

#[test]
fn inherited_default_requires_exact_leaf_override_and_legal_flags() {
    for mode in 0..3 {
        let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let parent = parent("Lsample/Parent;");
        let mut contract = interface("Lsample/Contract;", "run");
        let contract_owner = contract.descriptor.clone();
        let leaf_owner = leaf.descriptor.clone();
        contract.methods[0] = concrete_method(&contract_owner, "run");
        leaf.interfaces.push(contract.descriptor.clone());
        if mode == 1 {
            leaf.methods.push(concrete_method(&leaf_owner, "run"));
        } else if mode == 2 {
            contract.methods[0].access_flags = 0;
        }
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&leaf, &parent, &contract, &caller]);
        assert_eq!(
            hierarchy.recovered_constructors(&leaf.descriptor).len(),
            if mode == 1 { 1 } else { 0 }
        );
    }
}

#[test]
fn final_parent_method_collision_declines() {
    let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let mut parent = parent("Lsample/Parent;");
    let parent_owner = parent.descriptor.clone();
    let leaf_owner = leaf.descriptor.clone();
    let mut inherited = concrete_method(&parent_owner, "run");
    inherited.access_flags = 0x11;
    parent.methods.push(inherited);
    leaf.methods.push(concrete_method(&leaf_owner, "run"));
    let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
    let hierarchy = install(&[&leaf, &parent, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );
}

#[test]
fn inherited_interface_diamond_is_complete_without_being_a_cycle() {
    for reverse in [false, true] {
        let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let parent = parent("Lsample/Parent;");
        let mut left = interface("Lsample/Left;", "unusedLeft");
        let mut right = interface("Lsample/Right;", "unusedRight");
        let shared = interface("Lsample/Shared;", "run");
        left.methods.clear();
        right.methods.clear();
        left.interfaces.push(shared.descriptor.clone());
        right.interfaces.push(shared.descriptor.clone());
        leaf.interfaces.push(left.descriptor.clone());
        leaf.interfaces.push(right.descriptor.clone());
        let leaf_owner = leaf.descriptor.clone();
        leaf.methods.push(concrete_method(&leaf_owner, "run"));
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = if reverse {
            install(&[&caller, &shared, &right, &left, &parent, &leaf])
        } else {
            install(&[&leaf, &parent, &left, &right, &shared, &caller])
        };
        assert_eq!(hierarchy.recovered_constructors(&leaf.descriptor).len(), 1);
    }
}

#[test]
fn cyclic_inherited_interfaces_decline_owner_wide() {
    for self_cycle in [false, true] {
        for reverse in [false, true] {
            let mut leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
            let parent = parent("Lsample/Parent;");
            let mut first = interface("Lsample/First;", "unusedFirst");
            let mut second = interface("Lsample/Second;", "unusedSecond");
            first.methods.clear();
            second.methods.clear();
            if self_cycle {
                first.interfaces.push(first.descriptor.clone());
            } else {
                first.interfaces.push(second.descriptor.clone());
                second.interfaces.push(first.descriptor.clone());
            }
            leaf.interfaces.push(first.descriptor.clone());
            let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
            let hierarchy = if reverse {
                install(&[&caller, &second, &first, &parent, &leaf])
            } else {
                install(&[&leaf, &parent, &first, &second, &caller])
            };
            assert!(
                hierarchy
                    .recovered_constructors(&leaf.descriptor)
                    .is_empty()
            );
        }
    }
}

#[test]
fn nonordinary_parent_constructor_flags_decline() {
    for flag in [0x10, 0x20, 0x40, 0x80] {
        let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
        let mut parent = parent("Lsample/Parent;");
        parent.methods[0].access_flags |= flag;
        let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
        let hierarchy = install(&[&leaf, &parent, &caller]);
        assert!(
            hierarchy
                .recovered_constructors(&leaf.descriptor)
                .is_empty()
        );
    }
}

#[test]
fn package_leaf_requires_every_allocation_caller_in_its_package() {
    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let parent_class = parent("Lsample/Parent;");
    let mut caller = exact_caller(&leaf.descriptor, &parent_class.descriptor);
    caller.descriptor = "Lother/Factory;".into();
    caller.methods[0].declaring_type = caller.descriptor.clone();
    let hierarchy = install(&[&leaf, &parent_class, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );

    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 1);
    let parent_class2 = parent("Lsample/Parent;");
    let mut caller = exact_caller(&leaf.descriptor, &parent_class2.descriptor);
    caller.descriptor = "Lother/Factory;".into();
    caller.methods[0].declaring_type = caller.descriptor.clone();
    let hierarchy = install(&[&leaf, &parent_class2, &caller]);
    assert_eq!(hierarchy.recovered_constructors(&leaf.descriptor).len(), 1);
}

#[test]
fn duplicate_owner_and_malformed_nesting_decline() {
    let leaf = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let duplicate = class("Lsample/Leaf$4;", "Lsample/Parent;", 0);
    let parent = parent("Lsample/Parent;");
    let caller = exact_caller(&leaf.descriptor, &parent.descriptor);
    let hierarchy = install(&[&leaf, &duplicate, &parent, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );

    let mut malformed = anonymous("Lsample/Outer$1;", "Lsample/Outer;", OBJECT);
    let directory = Arc::get_mut(&mut malformed.symbols)
        .unwrap()
        .annotations
        .get_mut(&1)
        .unwrap();
    let directory = Arc::get_mut(directory).unwrap();
    let mut set = directory.class.as_ref().unwrap().to_vec();
    set.push(set[0].clone());
    directory.class = Some(set.into());
    let caller = exact_caller(&malformed.descriptor, OBJECT);
    let hierarchy = install(&[&malformed, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&malformed.descriptor)
            .is_empty()
    );
}

#[test]
fn loaded_object_revokes_platform_parent_authority() {
    let leaf = class("Lsample/Leaf$4;", OBJECT, 0);
    let mut shadow = class(OBJECT, OBJECT, 1);
    shadow.superclass = None;
    let caller = exact_caller(&leaf.descriptor, OBJECT);
    let hierarchy = install(&[&leaf, &shadow, &caller]);
    assert!(
        hierarchy
            .recovered_constructors(&leaf.descriptor)
            .is_empty()
    );
}

#[test]
#[ignore = "requires root-pinned JDK execution"]
fn modeled_jvm_fault_order_and_default_state_match() {
    let status = std::process::Command::new("python3")
        .arg(format!(
            "{}/tests/fixtures/ctorless_flat_noarg_jvm.py",
            env!("CARGO_MANIFEST_DIR")
        ))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn final_public_interface_implementations_supply_exact_flat_constructor_obligations() {
    for flags in [1, 0x11, 0x1041, 0x1051, 9, 2, 0x401] {
        let mut leaf = class("Lsample/FinalImpl$1;", OBJECT, 1);
        let obligation = interface("Lsample/Obligation;", "apply");
        leaf.interfaces.push(obligation.descriptor.clone());
        let mut implementation = concrete_method(&leaf.descriptor, "apply");
        implementation.access_flags = flags;
        if flags == 0x401 {
            implementation.code = None;
        }
        leaf.methods.push(implementation);
        let caller = exact_caller(&leaf.descriptor, OBJECT);
        let hierarchy = install(&[&leaf, &obligation, &caller]);
        assert_eq!(
            hierarchy.recovered_constructors(&leaf.descriptor).len(),
            usize::from(matches!(flags, 1 | 0x11 | 0x1041 | 0x1051)),
            "flags {flags:x}"
        );
    }
}

#[test]
#[ignore = "root runs javac/java serially with RDX_FLAT_SUPER_JVM_DIR"]
fn jvm_final_public_flat_implementation_preserves_interface_dispatch() {
    let mut leaf = class("Lsample/FinalImpl$1;", OBJECT, 1);
    let obligation = interface("Lsample/Obligation;", "apply");
    leaf.interfaces.push(obligation.descriptor.clone());
    let mut implementation = concrete_method(&leaf.descriptor, "apply");
    implementation.access_flags = 0x11;
    leaf.methods.push(implementation);
    let caller = exact_caller(&leaf.descriptor, OBJECT);
    install(&[&leaf, &obligation, &caller]);
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_FLAT_SUPER_JVM_DIR").expect("RDX_FLAT_SUPER_JVM_DIR"),
    )
    .join("final-implementation");
    let package = base.join("sample");
    std::fs::create_dir_all(&package).unwrap();
    for (name, c) in [
        ("FinalImpl$1", &leaf),
        ("Obligation", &obligation),
        ("Factory", &caller),
    ] {
        std::fs::write(
            package.join(format!("{name}.java")),
            native_java::render(&format!("sample.{name}"), c)
                .unwrap()
                .source,
        )
        .unwrap();
    }
    std::fs::write(package.join("Check.java"),r#"package sample;
public class Check { public static void main(String[] args) throws Exception {
    Factory.make(); Obligation value = new FinalImpl$1(); value.apply();
    if (!java.lang.reflect.Modifier.isFinal(FinalImpl$1.class.getDeclaredMethod("apply").getModifiers())) throw new AssertionError();
    System.out.print("ok");
} }
"#).unwrap();
    let javac = std::process::Command::new("javac")
        .arg("-d")
        .arg(&base)
        .args([
            package.join("FinalImpl$1.java"),
            package.join("Obligation.java"),
            package.join("Factory.java"),
            package.join("Check.java"),
        ])
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = std::process::Command::new("java")
        .arg("-cp")
        .arg(&base)
        .arg("sample.Check")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok");
}
