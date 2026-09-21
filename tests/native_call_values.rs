use rdx::native_call_values::SsaCalls;
use rdx::native_calls::BoundCalls;
use rdx::native_cfg::ControlFlowGraph;
use rdx::native_dex::{DexCode, DexSymbols};
use rdx::native_ir::DecodedMethod;
use rdx::native_ssa::{DefinitionKind, SsaMethod};

#[test]
fn call_types_attach_to_reaching_values_before_result_overwrites_receiver() {
    // Input v0 is reused for the result. Both occurrences of v1 must bind to
    // the same input value; v2/v3 are one wide argument. Result gets a new ID.
    let code = DexCode {
        registers: 4,
        ins: 4,
        outs: 5,
        tries: 0,
        try_regions: vec![],
        offset: 0,
        instructions: vec![0x536e, 0, 0x2110, 0x000c, 0x0011],
    };
    let symbols = DexSymbols {
        types: vec!["LTarget;".into()],
        strings: vec!["call".into()],
        protos: vec![("LTarget;".into(), vec!["I".into(), "I".into(), "J".into()])],
        methods: vec![(0, 0, 0)],
        ..DexSymbols::default()
    };
    let ir = DecodedMethod::decode(&code).unwrap();
    let graph = ControlFlowGraph::build(&code).unwrap();
    let ssa = SsaMethod::build(&code, &ir, &graph).unwrap();
    let bound = BoundCalls::bind(&code, &ir, &symbols).unwrap();
    let values = SsaCalls::bind(&bound, &ssa).unwrap();
    let call = &values.calls[0];
    assert_eq!(call.arguments[0], call.arguments[1]);
    assert_eq!(call.arguments[2].words.len(), 2);
    let receiver = call.receiver.as_ref().unwrap().words[0];
    let result = call.result.as_ref().unwrap().words[0];
    assert_ne!(receiver, result);
    assert_eq!(ssa.definitions[receiver].kind, DefinitionKind::Parameter);
    assert!(matches!(
        ssa.definitions[result].kind,
        DefinitionKind::Instruction { pc: 3, .. }
    ));
    assert_eq!(ssa.instructions.last().unwrap().reads[0].words[0], result);
    assert_eq!(values.unreachable_calls, 0);
}

#[test]
fn dead_calls_are_explicitly_counted_and_bad_binding_is_rejected() {
    let code = DexCode {
        registers: 1,
        ins: 1,
        outs: 1,
        tries: 0,
        try_regions: vec![],
        offset: 0,
        instructions: vec![0x000e, 0x1071, 0, 0, 0x000e],
    };
    let symbols = DexSymbols {
        types: vec!["LTarget;".into()],
        strings: vec!["call".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..DexSymbols::default()
    };
    let ir = DecodedMethod::decode(&code).unwrap();
    let ssa = SsaMethod::build(&code, &ir, &ControlFlowGraph::build(&code).unwrap()).unwrap();
    let mut bound = BoundCalls::bind(&code, &ir, &symbols).unwrap();
    let linked = SsaCalls::bind(&bound, &ssa).unwrap();
    assert_eq!(linked.unreachable_calls, 1);
    assert!(linked.calls.is_empty());
    bound.calls[0].pc = 0;
    assert!(SsaCalls::bind(&bound, &ssa).is_err());
}
