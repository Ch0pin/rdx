use rdx::native_call_values::SsaCalls;
use rdx::native_calls::BoundCalls;
use rdx::native_cfg::ControlFlowGraph;
use rdx::native_constructors::{ConstructorAnalysis, ConstructorOrigin};
use rdx::native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion};
use rdx::native_ir::DecodedMethod;
use rdx::native_ssa::SsaMethod;
use std::sync::Arc;

fn class(words: &[u16], registers: u16, ins: u16, init: bool) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into(), "effect".into()],
            types: vec![Arc::from("LTarget;"), Arc::from("LSuper;")],
            protos: vec![(Arc::from("V"), vec![])],
            methods: vec![(0, 0, 0), (1, 0, 0), (0, 0, 1)],
            ..Default::default()
        }),
        descriptor: Arc::from("LTarget;"),
        superclass: Some(Arc::from("LSuper;")),
        interfaces: vec![],
        access_flags: 0,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: Arc::from("LTarget;"),
            name: Arc::from(if init { "<init>" } else { "test" }),
            return_type: Arc::from("V"),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: if init { 0 } else { 8 },
            code: Some(DexCode {
                registers,
                ins,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: words.to_vec(),
                offset: 0,
            }),
        }],
    }
}
fn analyze(class: &DexClass) -> ConstructorAnalysis {
    let m = &class.methods[0];
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    let cfg = ControlFlowGraph::build(code).unwrap();
    let ssa = SsaMethod::build(code, &ir, &cfg).unwrap();
    let bound = BoundCalls::bind(code, &ir, &class.symbols).unwrap();
    let calls = SsaCalls::bind(&bound, &ssa).unwrap();
    ConstructorAnalysis::analyze(class, m, &ir, &bound, &ssa, &calls).unwrap()
}
#[test]
fn aliases_keep_exact_allocation_across_effects_and_register_reuse() {
    // new v0; move-object v1,v0; effect(); move-object v2,v1;
    // overwrite v0; invoke-direct v2.<init>(); return.
    let c = class(
        &[
            0x0022, 0, 0x0107, 0x0071, 2, 0, 0x1207, 0x0012, 0x1070, 0, 2, 0x000e,
        ],
        3,
        0,
        false,
    );
    let original = c.methods[0].code.as_ref().unwrap().instructions.clone();
    let a = analyze(&c);
    assert!(a.unresolved.is_empty());
    assert_eq!(a.bindings.len(), 1);
    assert_eq!(a.bindings[0].invoke_pc, 8);
    assert_eq!(
        a.bindings[0].origin,
        ConstructorOrigin::Allocation {
            pc: 0,
            type_descriptor: Arc::from("LTarget;")
        }
    );
    assert_eq!(c.methods[0].code.as_ref().unwrap().instructions, original);
}
#[test]
fn this_and_direct_super_chaining_use_parameter_identity() {
    for (target, expected) in [(0, ConstructorOrigin::This), (1, ConstructorOrigin::Super)] {
        let c = class(&[0x1007, 0x1070, target, 0, 0x000e], 2, 1, true);
        let a = analyze(&c);
        assert!(a.unresolved.is_empty());
        assert_eq!(a.bindings[0].origin, expected);
    }
    let mut c = class(&[0x1070, 0, 0, 0x000e], 1, 1, true);
    c.methods[0].name = Arc::from("other");
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(a.unresolved.len(), 1);
}
#[test]
fn phi_of_distinct_allocations_is_unresolved() {
    // if-eqz v1 -> second allocation; else first allocation; join constructor.
    let c = class(
        &[
            0x0138, 5, 0x0022, 0, 0x0328, 0x0022, 0, 0x1070, 0, 0, 0x000e,
        ],
        2,
        1,
        false,
    );
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(a.unresolved.len(), 1);
    assert_eq!(a.unresolved[0].invoke_pc, 7);
    assert_eq!(
        a.unresolved[0].reason,
        "constructor receiver merges distinct allocation identities"
    );
}
#[test]
fn phi_of_aliases_of_same_allocation_resolves() {
    // new v0; branch v2; v1=v0 on either branch; join.
    let c = class(
        &[
            0x0022, 0, 0x0238, 4, 0x0107, 0x0228, 0x0107, 0x1070, 0, 1, 0x000e,
        ],
        3,
        1,
        false,
    );
    let a = analyze(&c);
    assert!(a.unresolved.is_empty());
    assert_eq!(
        a.bindings[0].origin,
        ConstructorOrigin::Allocation {
            pc: 0,
            type_descriptor: Arc::from("LTarget;")
        }
    );
}
#[test]
fn allocation_failure_handler_does_not_inherit_successful_value() {
    // protected new v0, normal constructor and return; handler constructor is invalid.
    let mut c = class(
        &[
            0x0022, 0, 0x1070, 0, 0, 0x000e, 0x010d, 0x1070, 0, 0, 0x000e,
        ],
        2,
        0,
        false,
    );
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions.push(DexTryRegion {
        start: 0,
        end: 2,
        catches: Arc::from([(None, 6)]),
    });
    let a = analyze(&c);
    assert_eq!(a.bindings.len(), 1);
    assert_eq!(a.bindings[0].invoke_pc, 2);
    assert_eq!(a.unresolved.len(), 1);
    assert_eq!(a.unresolved[0].invoke_pc, 7);
}
#[test]
fn owner_mismatch_is_not_guessed_and_unreachable_is_counted() {
    let c = class(&[0x0022, 0, 0x1070, 1, 0, 0x000e], 1, 0, false);
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(a.unresolved.len(), 1);
    let c = class(&[0x000e, 0x1070, 0, 0, 0x000e], 1, 0, false);
    let a = analyze(&c);
    assert_eq!(a.unreachable_constructors, 1);
    assert!(a.bindings.is_empty());
}
#[test]
fn explicit_work_limit_rejects_without_partial_success() {
    let c = class(&[0x0022, 0, 0x1070, 0, 0, 0x000e], 1, 0, false);
    let m = &c.methods[0];
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    let cfg = ControlFlowGraph::build(code).unwrap();
    let ssa = SsaMethod::build(code, &ir, &cfg).unwrap();
    let bound = BoundCalls::bind(code, &ir, &c.symbols).unwrap();
    let calls = SsaCalls::bind(&bound, &ssa).unwrap();
    assert!(
        ConstructorAnalysis::analyze_with_work_limit(&c, m, &ir, &bound, &ssa, &calls, 0).is_err()
    );
}

#[test]
fn cyclic_phi_with_one_allocation_seed_converges() {
    let c = class(
        &[
            0x0022, 0, 0x0107, 0x0238, 4, 0x1107, 0xfd28, 0x1070, 0, 1, 0x000e,
        ],
        3,
        1,
        false,
    );
    let a = analyze(&c);
    assert!(a.unresolved.is_empty());
    assert_eq!(a.bindings.len(), 1);
    assert_eq!(
        a.bindings[0].origin,
        ConstructorOrigin::Allocation {
            pc: 0,
            type_descriptor: Arc::from("LTarget;")
        }
    );
}

#[test]
fn proven_ancestor_owner_preserves_original_invoke_and_marks_retargeting() {
    use rdx::native_hierarchy::TypeHierarchy;
    let c = class(&[0x0022, 0, 0x1070, 1, 0, 0x000e], 1, 0, false);
    c.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c]).unwrap()))
        .unwrap();
    let a = analyze(&c);
    assert!(a.unresolved.is_empty());
    assert_eq!(a.bindings.len(), 1);
    let binding = &a.bindings[0];
    assert_eq!(binding.invoked_owner.as_ref(), "LSuper;");
    assert!(binding.owner_retarget_required);
    assert_eq!(
        binding.origin,
        ConstructorOrigin::Allocation {
            pc: 0,
            type_descriptor: Arc::from("LTarget;")
        }
    );
    // The actual method pool and invoke instruction remain unmodified.
    assert_eq!(c.symbols.methods[1].0, 1);
    assert_eq!(c.methods[0].code.as_ref().unwrap().instructions[3], 1);

    let exact = class(&[0x0022, 0, 0x1070, 0, 0, 0x000e], 1, 0, false);
    let a = analyze(&exact);
    assert_eq!(a.bindings[0].invoked_owner.as_ref(), "LTarget;");
    assert!(!a.bindings[0].owner_retarget_required);
}

#[test]
fn unknown_and_disproven_owner_relationships_remain_distinct() {
    use rdx::native_hierarchy::TypeHierarchy;
    let c = class(&[0x0022, 0, 0x1070, 1, 0, 0x000e], 1, 0, false);
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(
        a.unresolved[0].reason,
        "constructor owner relationship to allocation type is unknown"
    );
    let mut c = class(&[0x0022, 0, 0x1070, 1, 0, 0x000e], 1, 0, false);
    c.superclass = Some(Arc::from("Ljava/lang/Object;"));
    c.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c]).unwrap()))
        .unwrap();
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(
        a.unresolved[0].reason,
        "constructor owner incompatible with allocation type"
    );
}

#[test]
fn ancestor_constructor_on_this_preserves_invoked_owner_and_marks_retarget() {
    use rdx::native_hierarchy::TypeHierarchy;
    let mut c = class(&[0x1070, 1, 0, 0x000e], 1, 1, true);
    c.superclass = Some(Arc::from("LMiddle;"));
    let mut middle = class(&[0x000e], 1, 1, true);
    middle.descriptor = Arc::from("LMiddle;");
    // Middle -> Super is a recorded project relationship.
    c.symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&c, &middle]).unwrap(),
        ))
        .unwrap();
    let a = analyze(&c);
    assert!(a.unresolved.is_empty());
    assert_eq!(a.bindings.len(), 1);
    assert_eq!(a.bindings[0].origin, ConstructorOrigin::Super);
    assert_eq!(a.bindings[0].invoked_owner.as_ref(), "LSuper;");
    assert!(a.bindings[0].owner_retarget_required);
    assert_eq!(
        c.methods[0].code.as_ref().unwrap().instructions,
        [0x1070, 1, 0, 0x000e]
    );
}

#[test]
fn unproven_ancestor_constructor_on_this_is_not_classified_as_super() {
    use rdx::native_hierarchy::TypeHierarchy;
    let mut c = class(&[0x1070, 1, 0, 0x000e], 1, 1, true);
    c.superclass = Some(Arc::from("LUnavailable;"));
    c.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c]).unwrap()))
        .unwrap();
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(
        a.unresolved[0].reason,
        "chained constructor owner relationship to this class is unknown"
    );
    let mut c = class(&[0x1070, 1, 0, 0x000e], 1, 1, true);
    c.superclass = Some(Arc::from("Ljava/lang/Object;"));
    c.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c]).unwrap()))
        .unwrap();
    let a = analyze(&c);
    assert!(a.bindings.is_empty());
    assert_eq!(
        a.unresolved[0].reason,
        "chained constructor owner incompatible with this class"
    );
}
