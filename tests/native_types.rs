use rdx::{
    native_call_values::{CallValues, SsaCalls, TypedValue},
    native_cfg::ControlFlowGraph,
    native_dex::{DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_ir::DecodedMethod,
    native_ssa::SsaMethod,
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::sync::Arc;
fn method(words: &[u16], regs: u16, params: &[&str], ret: &str) -> DexMethod {
    DexMethod {
        declaring_type: Arc::from("LTest;"),
        name: Arc::from("m"),
        return_type: Arc::from(ret),
        parameters: params.iter().map(|s| Arc::from(*s)).collect(),
        thrown_types: vec![],
        access_flags: 8,
        code: Some(DexCode {
            registers: regs,
            ins: params
                .iter()
                .map(|s| if matches!(*s, "J" | "D") { 2 } else { 1 })
                .sum(),
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words.to_vec(),
            offset: 0,
        }),
    }
}
fn run(m: &DexMethod, symbols: &DexSymbols) -> (SsaMethod, InferredTypes) {
    let c = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(c).unwrap();
    let ssa = SsaMethod::build(c, &ir, &ControlFlowGraph::build(c).unwrap()).unwrap();
    let types = InferredTypes::infer(m, &ir, &ssa, &SsaCalls::default(), symbols).unwrap();
    (ssa, types)
}
fn written(s: &SsaMethod, pc: usize) -> usize {
    s.instructions.iter().find(|i| i.pc == pc).unwrap().writes[0].words[0]
}
#[test]
fn zero_resolves_boolean_or_null_by_use_not_register_number() {
    for ty in ["Z", "Ljava/lang/String;"] {
        let m = method(
            &[0x0012, if ty == "Z" { 0x000f } else { 0x0011 }],
            1,
            &[],
            ty,
        );
        let (s, t) = run(&m, &DexSymbols::default());
        assert_eq!(
            t.values[written(&s, 0)].resolution,
            TypeResolution::Resolved(Arc::from(ty))
        );
    }
    let m = method(&[0x0012, 0x001a, 0, 0x0011], 1, &[], "Ljava/lang/String;");
    let (s, t) = run(&m, &DexSymbols::default());
    assert!(matches!(
        t.values[written(&s, 0)].resolution,
        TypeResolution::Unresolved(_)
    ));
    assert_eq!(
        t.values[written(&s, 1)].resolution,
        TypeResolution::Resolved(Arc::from("Ljava/lang/String;"))
    );
}
#[test]
fn literal_bits_keep_conflicting_interpretations_unresolved() {
    let m = method(&[0x0012, 0x000f], 1, &[], "I");
    let c = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(c).unwrap();
    let ssa = SsaMethod::build(c, &ir, &ControlFlowGraph::build(c).unwrap()).unwrap();
    let value = written(&ssa, 0);
    let calls = SsaCalls {
        calls: vec![CallValues {
            pc: 1,
            receiver: None,
            arguments: vec![TypedValue {
                descriptor: Arc::from("F"),
                words: vec![value],
            }],
            result: None,
        }],
        unreachable_calls: 0,
    };
    let t = InferredTypes::infer(&m, &ir, &ssa, &calls, &DexSymbols::default()).unwrap();
    assert_eq!(
        t.values[value].resolution,
        TypeResolution::Unresolved("literal has multiple use types")
    );
    assert_eq!(t.conflicting_values, 0);
}
#[test]
fn generic_reference_use_cannot_disappear_when_selecting_literal_type() {
    let m = method(&[0x0012, 0x0121, 0x000f], 2, &[], "I");
    let (s, t) = run(&m, &DexSymbols::default());
    assert!(matches!(
        t.values[written(&s, 0)].resolution,
        TypeResolution::Unresolved(_)
    ));
}
#[test]
fn phi_preserves_unknown_incoming_producer() {
    // Branch over an unbound result producer, then merge with a constant.
    let m = method(
        &[
            0x0238, 7, 0x000a, 0x0000, 0x0428, 0x0000, 0x0000, 0x1012, 0x000f,
        ],
        3,
        &["[I", "I"],
        "I",
    );
    let (s, t) = run(&m, &DexSymbols::default());
    assert!(!s.phis.is_empty());
    let p = &s.phis[0];
    assert!(
        t.values[p.result]
            .assignment
            .contains(&AssignmentBound::Unknown)
    );
    assert!(matches!(
        t.values[p.result].resolution,
        TypeResolution::Unresolved(_)
    ));
}
#[test]
fn move_and_phi_propagate_use_requirements() {
    let m = method(
        &[0x0238, 4, 0x0012, 0x0228, 0x1012, 0x0101, 0x010f],
        3,
        &["Z"],
        "Z",
    );
    let (s, t) = run(&m, &DexSymbols::default());
    assert_eq!(t.conflicting_values, 0);
    assert_eq!(
        t.values[written(&s, 5)].resolution,
        TypeResolution::Resolved(Arc::from("Z"))
    );
}
#[test]
fn incompatible_primitive_assignment_is_conflict() {
    let m = method(&[0x000f], 1, &["F"], "I");
    let (_, t) = run(&m, &DexSymbols::default());
    assert_eq!(t.conflicting_values, 1);
}
#[test]
fn missing_hierarchy_stays_unresolved() {
    let m = method(&[0x0011], 1, &["LChild;"], "LParent;");
    let (_, t) = run(&m, &DexSymbols::default());
    assert_eq!(t.unresolved_values, 1);
    assert_eq!(t.conflicting_values, 0);
}
#[test]
fn wide_pairs_use_signature_boundaries_and_call_grouping() {
    for (params, registers, words, expected) in [
        (vec!["J"], 2, vec![0, 1], 0),
        (vec!["I", "I"], 2, vec![0, 1], 1),
        (vec!["J", "J"], 4, vec![1, 2], 1),
    ] {
        let m = method(&[0x000e], registers, &params, "V");
        let c = m.code.as_ref().unwrap();
        let ir = DecodedMethod::decode(c).unwrap();
        let ssa = SsaMethod::build(c, &ir, &ControlFlowGraph::build(c).unwrap()).unwrap();
        let calls = SsaCalls {
            calls: vec![CallValues {
                pc: 0,
                receiver: None,
                arguments: vec![TypedValue {
                    descriptor: Arc::from("J"),
                    words,
                }],
                result: None,
            }],
            unreachable_calls: 0,
        };
        let t = InferredTypes::infer(&m, &ir, &ssa, &calls, &DexSymbols::default()).unwrap();
        assert_eq!(t.wide_pair_issues, expected);
    }
}
#[test]
fn partial_wide_overwrite_is_not_coherent() {
    let m = method(&[0x0016, 0, 0x0112, 0x0010], 2, &[], "J");
    let (_, t) = run(&m, &DexSymbols::default());
    assert!(t.wide_pair_issues > 0);
}
#[test]
fn typed_catch_uses_actual_exception_bound() {
    let mut m = method(
        &[0x0012, 0x0027, 0x000d, 0x0011],
        1,
        &[],
        "Ljava/lang/Exception;",
    );
    let c = m.code.as_mut().unwrap();
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 1,
        end: 2,
        catches: Arc::from([(Some(Arc::from("Ljava/lang/Exception;")), 2)]),
    }];
    let (s, t) = run(&m, &DexSymbols::default());
    assert_eq!(
        t.values[written(&s, 2)].resolution,
        TypeResolution::Resolved(Arc::from("Ljava/lang/Exception;"))
    );
}
#[test]
fn boolean_bitwise_result_is_not_invented_integer() {
    let m = method(&[0x0097, 0x0100, 0x000f], 2, &["Z", "Z"], "Z");
    let (s, t) = run(&m, &DexSymbols::default());
    assert!(matches!(
        t.values[written(&s, 0)].resolution,
        TypeResolution::Unresolved(_)
    ));
    assert_eq!(t.conflicting_values, 0);
}
#[test]
fn resource_limits_and_unused_undefined_seeds() {
    let m = method(&[0x000e], 4, &[], "V");
    let c = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(c).unwrap();
    let ssa = SsaMethod::build(c, &ir, &ControlFlowGraph::build(c).unwrap()).unwrap();
    let t =
        InferredTypes::infer(&m, &ir, &ssa, &SsaCalls::default(), &DexSymbols::default()).unwrap();
    assert_eq!(t.unresolved_values, 0);
    assert!(
        InferredTypes::infer_with_work_limit(
            &m,
            &ir,
            &ssa,
            &SsaCalls::default(),
            &DexSymbols::default(),
            0
        )
        .is_err()
    );
}

#[test]
fn reference_phi_selects_proven_incoming_supertype_without_retyping_child() {
    use rdx::native_dex::DexClass;
    use rdx::native_hierarchy::TypeHierarchy;
    let class = |descriptor: &str, parent: &str| DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 0,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    let base = class("LBase;", "Ljava/lang/Object;");
    let child = class("LChild;", "LBase;");
    let symbols = DexSymbols::default();
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&base, &child]).unwrap(),
        ))
        .unwrap();
    let m = method(
        &[0x0338, 4, 0x1007, 0x0228, 0x2007, 0x0011],
        4,
        &["LBase;", "LChild;", "Z"],
        "LBase;",
    );
    let (ssa, types) = run(&m, &symbols);
    assert_eq!(
        types.values[ssa.phis[0].result].resolution,
        TypeResolution::Resolved("LBase;".into())
    );
    assert_eq!(
        types.values[written(&ssa, 4)].resolution,
        TypeResolution::Resolved("LChild;".into())
    );
    let (ssa, unknown) = run(&m, &DexSymbols::default());
    assert!(matches!(
        unknown.values[ssa.phis[0].result].resolution,
        TypeResolution::Unresolved(_)
    ));
}

#[test]
fn char_and_negative_sentinel_phi_promotes_to_int() {
    let m = method(
        &[0x0238, 5, 0x1001, 0x0328, 0x0000, 0xf012, 0x000f],
        3,
        &["C", "Z"],
        "I",
    );
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert_eq!(ssa.phis.len(), 1);
    assert_eq!(
        types.values[ssa.phis[0].result].resolution,
        TypeResolution::Resolved(Arc::from("I"))
    );
    assert_eq!(types.conflicting_values, 0);
}
#[test]
fn proven_reference_downcast_remains_an_explicit_conversion_requirement() {
    let m = method(
        &[0x0011],
        1,
        &["Ljava/lang/Object;"],
        "Ljava/lang/Throwable;",
    );
    let symbols = DexSymbols::default();
    symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes(std::iter::empty()).unwrap(),
        ))
        .unwrap();
    let (_, types) = run(&m, &symbols);
    assert_eq!(
        types.values[0].resolution,
        TypeResolution::Unresolved("reference narrowing conversion required")
    );
    assert_eq!(types.conflicting_values, 0);
}

#[test]
fn array_loads_resolve_components_and_wide_pairs() {
    for (array, op, component) in [
        ("[I", 0x44, "I"),
        ("[F", 0x44, "F"),
        ("[J", 0x45, "J"),
        ("[D", 0x45, "D"),
        ("[Z", 0x47, "Z"),
        ("[B", 0x48, "B"),
        ("[C", 0x49, "C"),
        ("[S", 0x4a, "S"),
        ("[Ljava/lang/String;", 0x46, "Ljava/lang/String;"),
    ] {
        let ret = if matches!(component, "J" | "D") {
            0x0010
        } else if component.starts_with('L') {
            0x0011
        } else {
            0x000f
        };
        // Parameter v2 -> v0 move, then get writes v0 (and v1 for wide).
        let m = method(&[0x2007, op, 0x0300, ret], 4, &[array, "I"], component);
        let (ssa, types) = run(&m, &DexSymbols::default());
        let output = ssa.instructions.iter().find(|i| i.pc == 1).unwrap();
        for word in &output.writes[0].words {
            assert_eq!(
                types.values[*word].resolution,
                TypeResolution::Resolved(component.into()),
                "{array}"
            );
        }
        assert_eq!(types.wide_pair_issues, 0);
    }
}

#[test]
fn nested_array_loads_reach_fixed_point_without_unknown_seed() {
    let m = method(
        &[0x0046, 0x0201, 0x0044, 0x0200, 0x000f],
        3,
        &["[[I", "I"],
        "I",
    );
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert_eq!(
        types.values[written(&ssa, 0)].resolution,
        TypeResolution::Resolved("[I".into())
    );
    assert_eq!(
        types.values[written(&ssa, 2)].resolution,
        TypeResolution::Resolved("I".into())
    );
}

#[test]
fn array_store_primitives_type_literal_and_object_store_preserves_runtime_check() {
    let m = method(&[0x1012, 0x004e, 0x0201, 0x000e], 3, &["[Z", "I"], "V");
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert_eq!(
        types.values[written(&ssa, 0)].resolution,
        TypeResolution::Resolved("Z".into())
    );
    let m = method(
        &[0x024d, 0x0100, 0x000e],
        3,
        &["[Ljava/lang/String;", "I", "Ljava/lang/Object;"],
        "V",
    );
    let (ssa, types) = run(&m, &DexSymbols::default());
    let object = ssa.instructions[0].reads[2].words[0];
    assert_eq!(
        types.values[object].resolution,
        TypeResolution::Resolved("Ljava/lang/Object;".into())
    );
    assert!(
        !types.values[object]
            .required_types
            .iter()
            .any(|t| t.as_ref() == "Ljava/lang/String;")
    );
}

#[test]
fn array_opcode_mismatch_and_array_length_non_array_are_not_accepted() {
    for (words, ty) in [
        (vec![0x0046, 0x0201, 0x000e], "[I"),
        (vec![0x0021, 0x000f], "Ljava/lang/Object;"),
    ] {
        let m = if words.len() == 2 {
            method(&words, 1, &[ty], "I")
        } else {
            method(&words, 3, &[ty, "I"], "V")
        };
        let (_, types) = run(&m, &DexSymbols::default());
        assert!(types.conflicting_values > 0);
    }
    let m = method(&[0x1021, 0x000f], 2, &["[J"], "I");
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert_eq!(
        types.values[written(&ssa, 0)].resolution,
        TypeResolution::Resolved("I".into())
    );
    assert_eq!(types.conflicting_values, 0);
}

#[test]
fn null_array_alternative_does_not_override_successful_component() {
    // v0 = flag ? array : null; v0 = v0[index]
    let m = method(
        &[0x0338, 4, 0x1007, 0x0228, 0x0012, 0x0044, 0x0200, 0x000f],
        4,
        &["[I", "I", "Z"],
        "I",
    );
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert_eq!(
        types.values[written(&ssa, 5)].resolution,
        TypeResolution::Resolved("I".into())
    );
    // Null-only arrays have no successful element assignment to infer.
    let m = method(&[0x0012, 0x0044, 0x0100, 0x000f], 2, &["I"], "I");
    let (ssa, types) = run(&m, &DexSymbols::default());
    assert!(matches!(
        types.values[written(&ssa, 1)].resolution,
        TypeResolution::Unresolved(_)
    ));
}

#[test]
fn unknown_array_join_cannot_be_resolved_from_known_branch() {
    // Unbound move-result is an unknown producer for this isolated inference test.
    let m = method(
        &[0x0338, 4, 0x1007, 0x0228, 0x000c, 0x0044, 0x0200, 0x000f],
        4,
        &["[I", "I", "Z"],
        "I",
    );
    let (ssa, types) = run(&m, &DexSymbols::default());
    let result = &types.values[written(&ssa, 5)];
    assert!(result.assignment.contains(&AssignmentBound::Unknown));
    assert!(matches!(result.resolution, TypeResolution::Unresolved(_)));
}

#[test]
fn filled_array_call_binding_provides_dynamic_component() {
    use rdx::native_calls::BoundCalls;
    let m = method(
        &[0x1024, 0, 2, 0x000c, 0x0044, 0x0200, 0x000f],
        3,
        &["I"],
        "I",
    );
    let symbols = DexSymbols {
        types: vec![Arc::from("[I")],
        ..DexSymbols::default()
    };
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    let ssa = SsaMethod::build(code, &ir, &ControlFlowGraph::build(code).unwrap()).unwrap();
    let calls = SsaCalls::bind(&BoundCalls::bind(code, &ir, &symbols).unwrap(), &ssa).unwrap();
    let types = InferredTypes::infer(&m, &ir, &ssa, &calls, &symbols).unwrap();
    assert_eq!(
        types.values[written(&ssa, 4)].resolution,
        TypeResolution::Resolved("I".into())
    );
}

#[test]
fn invoke_array_result_is_available_to_element_listener() {
    use rdx::native_calls::BoundCalls;
    let m = method(
        &[0x0071, 0, 0, 0x000c, 0x0046, 0x0100, 0x0011],
        2,
        &["I"],
        "Ljava/lang/String;",
    );
    let symbols = DexSymbols {
        types: vec!["LFactory;".into()],
        strings: vec!["strings".into()],
        protos: vec![("[Ljava/lang/String;".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..DexSymbols::default()
    };
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    let ssa = SsaMethod::build(code, &ir, &ControlFlowGraph::build(code).unwrap()).unwrap();
    let calls = SsaCalls::bind(&BoundCalls::bind(code, &ir, &symbols).unwrap(), &ssa).unwrap();
    let types = InferredTypes::infer(&m, &ir, &ssa, &calls, &symbols).unwrap();
    assert_eq!(
        types.values[written(&ssa, 4)].resolution,
        TypeResolution::Resolved("Ljava/lang/String;".into())
    );
}

#[test]
fn late_array_alternatives_and_truncation_propagate_to_element_results() {
    // Synthetic SSA join with many incoming array-typed call results exercises
    // listener ordering and the bound cap independently of CFG construction.
    use rdx::native_ssa::{Definition, DefinitionKind, Phi};
    let m = method(&[0x0046, 0x0201, 0x000e], 3, &["[LFirst;", "I"], "V");
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    for alternatives in [2, 10] {
        let mut ssa = SsaMethod::build(code, &ir, &ControlFlowGraph::build(code).unwrap()).unwrap();
        let mut calls = SsaCalls::default();
        let mut incoming = Vec::new();
        for n in 0..alternatives {
            let id = ssa.definitions.len();
            ssa.definitions.push(Definition {
                register: 1,
                kind: DefinitionKind::Instruction {
                    pc: n + 100,
                    word: 0,
                    block: 0,
                },
            });
            incoming.push((Some(n), id));
            calls.calls.push(CallValues {
                pc: n + 100,
                receiver: None,
                arguments: vec![],
                result: Some(TypedValue {
                    descriptor: Arc::from(format!("[LT{n};")),
                    words: vec![id],
                }),
            });
        }
        let result = ssa.definitions.len();
        ssa.definitions.push(Definition {
            register: 1,
            kind: DefinitionKind::Phi { block: 0 },
        });
        ssa.phis.push(Phi {
            block: 0,
            register: 1,
            result,
            incoming,
        });
        ssa.instructions[0].reads[0].words = vec![result];
        let types = InferredTypes::infer(&m, &ir, &ssa, &calls, &DexSymbols::default()).unwrap();
        let element = &types.values[written(&ssa, 0)];
        assert!(matches!(element.resolution, TypeResolution::Unresolved(_)));
        assert_eq!(element.bounds_truncated, alternatives > 8);
        assert!(!element.assignment.contains(&AssignmentBound::Unknown));
        assert_eq!(element.assignment.len(), alternatives.min(8));
    }
}

#[test]
fn wide_array_store_constrains_both_literal_words_without_retyping_array() {
    for descriptor in ["J", "D"] {
        let array = format!("[{descriptor}");
        let m = method(&[0x0016, 0, 0x004c, 0x0302, 0x000e], 4, &[&array, "I"], "V");
        let (ssa, types) = run(&m, &DexSymbols::default());
        for word in &ssa.instructions[0].writes[0].words {
            assert_eq!(
                types.values[*word].resolution,
                TypeResolution::Resolved(descriptor.into())
            );
        }
        assert_eq!(types.wide_pair_issues, 0);
        let array_word = ssa.instructions[1].reads[0].words[0];
        assert_eq!(
            types.values[array_word].resolution,
            TypeResolution::Resolved(array.clone().into())
        );
    }
}

#[test]
fn dynamic_array_cycle_without_a_concrete_seed_remains_unknown() {
    let m = method(
        &[0x0046, 0x0201, 0x000e],
        3,
        &["[Ljava/lang/Object;", "I"],
        "V",
    );
    let code = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(code).unwrap();
    let mut ssa = SsaMethod::build(code, &ir, &ControlFlowGraph::build(code).unwrap()).unwrap();
    let element = written(&ssa, 0);
    // A deliberately isolated dependency cycle tests the fixed-point solver;
    // it has no concrete incoming array descriptor and must not guess one.
    ssa.instructions[0].reads[0].words = vec![element];
    let types =
        InferredTypes::infer(&m, &ir, &ssa, &SsaCalls::default(), &DexSymbols::default()).unwrap();
    assert!(
        types.values[element]
            .assignment
            .contains(&AssignmentBound::Unknown)
    );
    assert!(matches!(
        types.values[element].resolution,
        TypeResolution::Unresolved(_)
    ));
}
