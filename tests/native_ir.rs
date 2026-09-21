use rdx::{
    native_dex::DexCode,
    native_ir::{DecodedMethod, PoolKind, ValueKind},
};

fn code(words: &[u16], registers: u16) -> DexCode {
    DexCode {
        registers,
        ins: 0,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        instructions: words.to_vec(),
        offset: 0,
    }
}

fn registers(operands: &[rdx::native_ir::RegisterOperand]) -> Vec<u16> {
    operands.iter().map(|arg| arg.register).collect()
}

#[test]
fn move_formats_and_wide_pairs_preserve_register_identity() {
    let ir = DecodedMethod::decode(&code(
        &[
            0x2101, 0x0302, 257, 0x0003, 300, 301, 0x4204, 0x8707, 0x000e,
        ],
        302,
    ))
    .unwrap();
    assert_eq!(registers(&ir.instructions[0].reads), [2]);
    assert_eq!(registers(&ir.instructions[0].writes), [1]);
    assert_eq!(registers(&ir.instructions[1].reads), [257]);
    assert_eq!(registers(&ir.instructions[2].writes), [300]);
    assert_eq!(ir.instructions[3].reads[0].kind, ValueKind::Wide64);
    assert_eq!(registers(&ir.instructions[3].reads), [4]);
    assert_eq!(registers(&ir.instructions[3].writes), [2]);
    assert_eq!(ir.instructions[4].reads[0].kind, ValueKind::Reference);
    assert!(DecodedMethod::decode(&code(&[0x0f04, 0x000e], 16)).is_err());
}

#[test]
fn cast_has_a_definition_and_preserves_type_reference() {
    let ir = DecodedMethod::decode(&code(&[0x031f, 123, 0x000e], 4)).unwrap();
    let cast = &ir.instructions[0];
    assert_eq!(registers(&cast.reads), [3]);
    assert_eq!(registers(&cast.writes), [3]);
    assert_eq!(cast.writes[0].kind, ValueKind::Reference);
    assert_eq!(cast.reference.as_ref().unwrap().kind, PoolKind::Type);
    assert_eq!(cast.reference.as_ref().unwrap().index, 123);
    assert!(cast.may_throw);
}

#[test]
fn long_shift_and_conversion_distinguish_narrow_and_wide_operands() {
    let ir = DecodedMethod::decode(&code(&[0x00a3, 0x0402, 0x2081, 0x20c3, 0x000e], 5)).unwrap();
    let shift = &ir.instructions[0];
    assert_eq!(registers(&shift.reads), [2, 4]);
    assert_eq!(shift.reads[0].kind, ValueKind::Wide64);
    assert_eq!(shift.reads[1].kind, ValueKind::Bits32);
    assert_eq!(shift.writes[0].kind, ValueKind::Wide64);
    assert_eq!(ir.instructions[1].writes[0].kind, ValueKind::Wide64);
    assert_eq!(registers(&ir.instructions[2].reads), [0, 2]);
    assert_eq!(registers(&ir.instructions[2].writes), [0]);
}

#[test]
fn array_and_field_effects_distinguish_values_from_receivers() {
    let ir = DecodedMethod::decode(&code(
        &[
            0x0246, 0x0100, 0x024d, 0x0100, 0x0253, 10, 0x025a, 10, 0x0268, 10, 0x000e,
        ],
        4,
    ))
    .unwrap();
    assert_eq!(registers(&ir.instructions[0].reads), [0, 1]);
    assert_eq!(ir.instructions[0].writes[0].kind, ValueKind::Reference);
    // Store value order is checked as a set: encoded operand order differs from use ordering.
    let mut uses = registers(&ir.instructions[1].reads);
    uses.sort_unstable();
    assert_eq!(uses, [0, 1, 2]);
    assert!(ir.instructions[1].writes.is_empty());
    assert_eq!(ir.instructions[2].writes[0].kind, ValueKind::Wide64);
    assert!(
        ir.instructions[3]
            .reads
            .iter()
            .any(|r| r.register == 2 && r.kind == ValueKind::Wide64)
    );
    assert_eq!(
        ir.instructions[4].reference.as_ref().unwrap().kind,
        PoolKind::Field
    );
    assert!(ir.instructions[..5].iter().all(|i| i.may_throw));
}

#[test]
fn invoke_formats_preserve_argument_words_and_duplicates_without_guessing_types() {
    let ir = DecodedMethod::decode(&code(
        &[
            0x546e, 42, 0x3210, 0x0377, 43, 7, 0x306e, 44, 0x0100, 0x000e,
        ],
        10,
    ))
    .unwrap();
    assert_eq!(registers(&ir.instructions[0].reads), [0, 1, 2, 3, 4]);
    assert_eq!(registers(&ir.instructions[1].reads), [7, 8, 9]);
    assert_eq!(registers(&ir.instructions[2].reads), [0, 0, 1]);
    assert!(
        ir.instructions[0]
            .reads
            .iter()
            .all(|r| r.kind == ValueKind::Unknown32)
    );
    assert!(ir.instructions[0].writes.is_empty());
    assert_eq!(
        ir.instructions[0].reference.as_ref().unwrap().kind,
        PoolKind::Method
    );
    assert!(DecodedMethod::decode(&code(&[0x606e, 0, 0, 0x000e], 16)).is_err());
    assert!(DecodedMethod::decode(&code(&[0x0377, 0, 8, 0x000e], 10)).is_err());
}

#[test]
fn literal_sign_extension_and_high_bits_are_exact() {
    let ir = DecodedMethod::decode(&code(
        &[
            0xf012, 0x0013, 0x8000, 0x0014, 0xffff, 0xffff, 0x0015, 0x8000, 0x0019, 0x8000, 0x0018,
            0xffff, 0xffff, 0xffff, 0xffff, 0x000e,
        ],
        2,
    ))
    .unwrap();
    let literals: Vec<_> = ir.instructions.iter().filter_map(|i| i.literal).collect();
    assert_eq!(literals, [-1, -32768, -1, i32::MIN as i64, i64::MIN, -1]);
}

#[test]
fn integer_division_can_throw_but_float_division_cannot() {
    let ir = DecodedMethod::decode(&code(
        &[0x0093, 0x0201, 0x00a9, 0x0201, 0x00db, 0x0101, 0x000e],
        3,
    ))
    .unwrap();
    assert!(ir.instructions[0].may_throw);
    assert!(!ir.instructions[1].may_throw);
    assert!(ir.instructions[2].may_throw);
}

#[test]
fn modern_references_preserve_polymorphic_prototype_and_call_site() {
    let ir = DecodedMethod::decode(&code(
        &[
            0x10fa, 11, 0, 12, 0x01fb, 13, 0, 14, 0x00fc, 15, 0, 0x00fe, 16, 0x00ff, 17, 0x000e,
        ],
        2,
    ))
    .unwrap();
    assert_eq!(ir.instructions[0].prototype, Some(12));
    assert_eq!(ir.instructions[1].prototype, Some(14));
    assert_eq!(registers(&ir.instructions[1].reads), [0]);
    assert_eq!(registers(&ir.instructions[3].writes), [0]);
    assert_eq!(ir.instructions[3].writes[0].kind, ValueKind::Reference);
    assert_eq!(registers(&ir.instructions[4].writes), [0]);
    assert_eq!(
        ir.instructions[2].reference.as_ref().unwrap().kind,
        PoolKind::CallSite
    );
    assert_eq!(
        ir.instructions[3].reference.as_ref().unwrap().kind,
        PoolKind::MethodHandle
    );
    assert_eq!(
        ir.instructions[4].reference.as_ref().unwrap().kind,
        PoolKind::Proto
    );
}

#[test]
fn branch_offsets_and_payloads_use_code_units_and_exact_boundaries() {
    let ir = DecodedMethod::decode(&code(&[0x0012, 0xff28], 1)).unwrap();
    assert_eq!(ir.instructions[1].branch_target, Some(0));
    assert!(DecodedMethod::decode(&code(&[0x0029, 1, 0x000e], 1)).is_err());
    let ir =
        DecodedMethod::decode(&code(&[0x002b, 4, 0, 0x000e, 0x0100, 1, 7, 0, 3, 0], 1)).unwrap();
    assert_eq!(ir.instructions.len(), 2);
    assert_eq!(ir.instructions[0].payload_target, Some(4));
    assert!(DecodedMethod::decode(&code(&[0x002b, 4, 0, 0x000e, 0x0300, 1, 0, 0], 1)).is_err());
}

#[test]
fn malformed_or_out_of_bounds_operands_fail_explicitly() {
    for words in [
        &[0x00e3][..],
        &[0x0018][..],
        &[0x010f][..],
        &[0x20fa, 0, 0][..],
    ] {
        assert!(
            DecodedMethod::decode(&code(words, 1)).is_err(),
            "{words:x?}"
        );
    }
}

#[test]
fn compare_long_requires_both_wide_pairs() {
    let ir = DecodedMethod::decode(&code(&[0x0031, 0x0301, 0x000e], 5)).unwrap();
    assert_eq!(registers(&ir.instructions[0].reads), [1, 3]);
    assert!(
        ir.instructions[0]
            .reads
            .iter()
            .all(|r| r.kind == ValueKind::Wide64)
    );
    assert_eq!(ir.instructions[0].writes[0].kind, ValueKind::Bits32);
    assert!(DecodedMethod::decode(&code(&[0x0031, 0x0301, 0x000e], 4)).is_err());
}

#[test]
fn decoding_limits_bound_range_argument_expansion() {
    // ~12k code units, but >1M argument words: stop before unbounded expansion.
    let mut words = Vec::new();
    for _ in 0..4_000 {
        words.extend([0xff77, 0, 0]);
    }
    words.push(0x000e);
    let error = DecodedMethod::decode(&code(&words, 255))
        .unwrap_err()
        .to_string();
    assert!(error.contains("operand budget"), "{error}");
}
