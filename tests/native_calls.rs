use rdx::native_calls::{BoundCalls, CallKind, CallTarget};
use rdx::native_dex::{DexCode, DexSymbols, DexTryRegion};
use rdx::native_ir::DecodedMethod;
use std::sync::Arc;

fn code(words: &[u16], registers: u16) -> DexCode {
    DexCode {
        registers,
        ins: 0,
        outs: 8,
        tries: 0,
        try_regions: vec![],
        instructions: words.to_vec(),
        offset: 0,
    }
}

fn symbols() -> DexSymbols {
    DexSymbols {
        strings: vec!["call".into(), "invokeExact".into()],
        types: vec![Arc::from("LTarget;"), Arc::from("[I")],
        protos: vec![
            (Arc::from("D"), vec![Arc::from("J"), Arc::from("I")]),
            (Arc::from("V"), vec![]),
            (Arc::from("Ljava/lang/Object;"), vec![Arc::from("J")]),
            (Arc::from("I"), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..DexSymbols::default()
    }
}

fn bind(code: &DexCode, symbols: &DexSymbols) -> anyhow::Result<BoundCalls> {
    let ir = DecodedMethod::decode(code)?;
    let bound = BoundCalls::bind(code, &ir, symbols)?;
    for call in &bound.calls {
        let instruction = ir
            .instructions
            .iter()
            .find(|insn| insn.pc == call.pc)
            .unwrap();
        if matches!(instruction.opcode, 0x6e..=0x72 | 0x74..=0x78) {
            let words: Vec<_> = instruction
                .reads
                .iter()
                .map(|word| usize::from(word.register))
                .collect();
            let direct = rdx::native_calls::bind_invocation(
                instruction.opcode,
                call.pc,
                instruction.reference.unwrap().index,
                &words,
                symbols,
            )?;
            let mut expected = call.clone();
            expected.result = None;
            assert_eq!(direct, expected);
        }
    }
    Ok(bound)
}

#[test]
fn compact_invoke_binds_receiver_wide_argument_and_wide_result() {
    // invoke-virtual {v1, v3, v4, v7}, method@0; move-result-wide v8
    let code = code(&[0x406e, 0, 0x7431, 0x080b, 0x000e], 10);
    let bound = bind(&code, &symbols()).unwrap();
    let call = &bound.calls[0];
    assert_eq!(call.kind, CallKind::Virtual);
    assert_eq!(call.receiver.as_ref().unwrap().register, 1);
    assert_eq!(
        call.receiver.as_ref().unwrap().descriptor.as_ref(),
        "LTarget;"
    );
    assert_eq!(call.arguments[0].register, 3);
    assert_eq!(call.arguments[0].descriptor.as_ref(), "J");
    assert_eq!(call.arguments[1].register, 7);
    assert_eq!(call.result.as_ref().unwrap().register.register, 8);
    assert_eq!(
        call.result.as_ref().unwrap().register.descriptor.as_ref(),
        "D"
    );
}

#[test]
fn range_invoke_consumes_physical_words_and_discarded_result_is_legal() {
    // invoke-static/range {v4..v6}, (J,I)D, with the result deliberately ignored.
    let code = code(&[0x0377, 0, 4, 0x000e], 8);
    let bound = bind(&code, &symbols()).unwrap();
    assert!(bound.calls[0].receiver.is_none());
    assert_eq!(
        bound.calls[0]
            .arguments
            .iter()
            .map(|a| a.register)
            .collect::<Vec<_>>(),
        [4, 6]
    );
    assert!(bound.calls[0].result.is_none());
}

#[test]
fn polymorphic_uses_secondary_prototype_but_retains_method_identity() {
    // Receiver v1, effective J argument v2/v3, secondary proto@2.
    let code = code(&[0x30fa, 1, 0x0321, 2, 0x040c, 0x000e], 5);
    let bound = bind(&code, &symbols()).unwrap();
    let call = &bound.calls[0];
    assert_eq!(call.kind, CallKind::Polymorphic);
    assert_eq!(call.arguments.len(), 1);
    assert_eq!(call.arguments[0].register, 2);
    assert_eq!(call.return_type.as_ref(), "Ljava/lang/Object;");
    assert!(matches!(&call.target, CallTarget::Method {
        method_index: 1, prototype_index: 2, name, ..
    } if name.as_ref() == "invokeExact"));
}

#[test]
fn array_owner_method_is_valid_and_binds_clone_result() {
    let mut syms = symbols();
    syms.protos.push((Arc::from("Ljava/lang/Object;"), vec![]));
    syms.methods.push((1, 4, 0));
    // invoke-virtual {v0}, [I.clone:()Ljava/lang/Object;; move-result-object v1
    let code = code(&[0x106e, 2, 0, 0x010c, 0x000e], 2);
    let bound = bind(&code, &syms).unwrap();
    let call = &bound.calls[0];
    assert_eq!(call.receiver.as_ref().unwrap().descriptor.as_ref(), "[I");
    assert_eq!(call.return_type.as_ref(), "Ljava/lang/Object;");
    assert_eq!(
        call.result.as_ref().unwrap().register.descriptor.as_ref(),
        "Ljava/lang/Object;"
    );
}

#[test]
fn void_wrong_kind_and_orphan_results_are_rejected() {
    let syms = symbols();
    // method@1 has the declared void prototype.
    let void_result = code(&[0x0071, 1, 0, 0x000a, 0x000e], 1);
    assert!(
        bind(&void_result, &syms)
            .unwrap_err()
            .to_string()
            .contains("void call")
    );

    // method@0 returns D, so a narrow move-result is invalid.
    let wrong_kind = code(&[0x3071, 0, 0x0210, 0x000a, 0x000e], 8);
    assert!(
        bind(&wrong_kind, &syms)
            .unwrap_err()
            .to_string()
            .contains("type mismatch")
    );

    let orphan = code(&[0x000a, 0x000e], 1);
    assert!(
        bind(&orphan, &syms)
            .unwrap_err()
            .to_string()
            .contains("orphan")
    );
}

#[test]
fn malformed_pool_bounds_descriptors_and_wide_pairs_are_rejected() {
    let missing_method = code(&[0x0071, 9, 0, 0x000e], 1);
    assert!(
        bind(&missing_method, &symbols())
            .unwrap_err()
            .to_string()
            .contains("method index")
    );

    let mut malformed = symbols();
    malformed.protos[3].0 = Arc::from("Q");
    malformed.methods[0].1 = 3;
    let bad_descriptor = code(&[0x0071, 0, 0, 0x000e], 1);
    assert!(
        format!("{:#}", bind(&bad_descriptor, &malformed).unwrap_err())
            .contains("malformed descriptor")
    );

    // Compact encoding supplies v2 and v4 for a J argument instead of v2/v3.
    let noncontiguous = code(&[0x30fa, 1, 0x0421, 2, 0x000e], 5);
    assert!(
        bind(&noncontiguous, &symbols())
            .unwrap_err()
            .to_string()
            .contains("contiguous")
    );

    let mut bad_declared_proto = symbols();
    bad_declared_proto.methods[1].1 = 99;
    let polymorphic = code(&[0x30fa, 1, 0x0321, 2, 0x000e], 5);
    assert!(
        bind(&polymorphic, &bad_declared_proto)
            .unwrap_err()
            .to_string()
            .contains("declared prototype index")
    );

    for owner in ["L/foo;", "Lfoo//Bar;"] {
        let mut bad_owner = symbols();
        bad_owner.types[0] = Arc::from(owner);
        let call = code(&[0x0071, 1, 0, 0x000e], 1);
        assert!(
            format!("{:#}", bind(&call, &bad_owner).unwrap_err()).contains("malformed descriptor")
        );
    }
}

#[test]
fn branch_or_handler_entry_into_move_result_is_rejected() {
    let syms = symbols();
    // invoke-static ()I; move-result v0; goto move-result; return-void
    let branch = code(&[0x0071, 0, 0, 0x000a, 0xff28, 0x000e], 1);
    let mut branch_syms = symbols();
    branch_syms.methods[0].1 = 3;
    assert!(
        bind(&branch, &branch_syms)
            .unwrap_err()
            .to_string()
            .contains("control flow enters")
    );

    let mut handler = code(&[0x0071, 0, 0, 0x000a, 0x000e], 1);
    handler.tries = 1;
    handler.try_regions.push(DexTryRegion {
        start: 0,
        end: 3,
        catches: Arc::from([(None, 3)]),
    });
    assert!(
        bind(&handler, &branch_syms)
            .unwrap_err()
            .to_string()
            .contains("control flow enters")
    );

    // packed-switch at pc 0 has a case targeting the move-result at pc 6.
    let switch = code(
        &[
            0x002b, 8, 0, 0x0071, 0, 0, 0x000a, 0x000e, 0x0100, 1, 0, 0, 6, 0,
        ],
        1,
    );
    assert!(
        bind(&switch, &branch_syms)
            .unwrap_err()
            .to_string()
            .contains("control flow enters")
    );
    drop(syms);
}

#[test]
fn filled_new_array_binds_elements_and_result() {
    let code = code(&[0x2024, 1, 0x0021, 0x030c, 0x000e], 4);
    let bound = bind(&code, &symbols()).unwrap();
    let call = &bound.calls[0];
    assert_eq!(call.kind, CallKind::FilledNewArray);
    assert_eq!(
        call.arguments
            .iter()
            .map(|a| a.register)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(call.arguments.iter().all(|a| a.descriptor.as_ref() == "I"));
    assert_eq!(
        call.result.as_ref().unwrap().register.descriptor.as_ref(),
        "[I"
    );
    assert!(matches!(
        call.target,
        CallTarget::Array { type_index: 1, .. }
    ));
}

#[test]
fn invoke_custom_is_explicitly_unsupported() {
    let code = code(&[0x00fc, 0, 0, 0x000e], 1);
    let error = bind(&code, &symbols()).unwrap_err().to_string();
    assert!(error.contains("invoke-custom"));
    assert!(error.contains("call-site metadata"));
}

#[test]
fn emitter_binding_rejects_malformed_operands_and_keeps_duplicates() {
    use rdx::native_calls::bind_invocation;
    let mut symbols = symbols();
    for registers in [&[1, 3, 5, 7][..], &[1, 3][..], &[1, 3, 4, 7, 8][..]] {
        assert!(bind_invocation(0x6e, 0, 0, registers, &symbols).is_err());
    }
    assert!(bind_invocation(0xff, 0, 0, &[], &symbols).is_err());
    assert!(bind_invocation(0x6e, 0, 99, &[], &symbols).is_err());
    assert!(bind_invocation(0x6e, 0, 0, &[65536, 3, 4, 7], &symbols).is_err());
    assert!(bind_invocation(0x71, 0, 0, &[65535, 65536, 7], &symbols).is_err());
    symbols.protos[0] = ("Ljava/lang/Object;".into(), vec!["I".into(), "I".into()]);
    let binding = bind_invocation(0x71, 12, 0, &[3, 3], &symbols).unwrap();
    assert_eq!(binding.arguments[0], binding.arguments[1]);
    assert_eq!(binding.return_type.as_ref(), "Ljava/lang/Object;");
    symbols.protos[0].1[0] = "V".into();
    assert!(bind_invocation(0x71, 0, 0, &[3, 3], &symbols).is_err());
}
