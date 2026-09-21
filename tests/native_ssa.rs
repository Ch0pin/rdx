use rdx::{
    native_cfg::ControlFlowGraph,
    native_dex::{DexCode, DexTryRegion},
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::sync::Arc;
fn code(words: &[u16], registers: u16, ins: u16) -> DexCode {
    DexCode {
        registers,
        ins,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        instructions: words.to_vec(),
        offset: 0,
    }
}
fn build(code: &DexCode) -> SsaMethod {
    SsaMethod::build(
        code,
        &DecodedMethod::decode(code).unwrap(),
        &ControlFlowGraph::build(code).unwrap(),
    )
    .unwrap()
}
fn read(ssa: &SsaMethod, pc: usize) -> usize {
    ssa.instructions.iter().find(|i| i.pc == pc).unwrap().reads[0].words[0]
}
fn write(ssa: &SsaMethod, pc: usize) -> usize {
    ssa.instructions.iter().find(|i| i.pc == pc).unwrap().writes[0].words[0]
}
#[test]
fn diamond_prunes_dead_register_and_merges_live_register() {
    let s = build(&code(&[0x0038, 4, 0x1112, 0x0228, 0x2112, 0x010f], 2, 1));
    assert_eq!(s.phis.len(), 1);
    let p = &s.phis[0];
    assert_eq!(p.register, 1);
    assert_eq!(read(&s, 5), p.result);
    let mut values: Vec<_> = p.incoming.iter().map(|v| v.1).collect();
    values.sort_unstable();
    let mut expected = vec![write(&s, 2), write(&s, 4)];
    expected.sort_unstable();
    assert_eq!(values, expected);
    let dead = build(&code(&[0x0038, 4, 0x1112, 0x0228, 0x2112, 0x000e], 2, 1));
    assert!(dead.phis.is_empty());
}
#[test]
fn entry_backedge_phi_preserves_initial_parameter() {
    // v0 += 1; if v0 != 0 goto entry; return v0
    let s = build(&code(&[0x00d8, 0x0100, 0x0039, 0xfffe, 0x000f], 1, 1));
    let p = s.phis.iter().find(|p| p.block == 0).unwrap();
    assert!(p.incoming.contains(&(None, 0)));
    assert!(p.incoming.iter().any(|v| v.1 == write(&s, 0)));
    assert_eq!(read(&s, 0), p.result);
    assert_eq!(s.definitions[0].kind, DefinitionKind::Parameter);
}
#[test]
fn handler_uses_prewrite_state_with_one_predecessor() {
    // const v0; array-length v0,v1; return v0; handler: move-exception v2; return v0
    let mut c = code(&[0x1012, 0x1021, 0x000f, 0x020d, 0x000f], 3, 1);
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 1,
        end: 2,
        catches: Arc::from([(Some(Arc::from("Ljava/lang/Exception;")), 3)]),
    }];
    let s = build(&c);
    assert_eq!(read(&s, 2), write(&s, 1));
    assert_eq!(read(&s, 4), write(&s, 0));
    assert_ne!(read(&s, 2), read(&s, 4));
    let DefinitionKind::Instruction { block, .. } = s.definitions[write(&s, 1)].kind else {
        panic!()
    };
    assert_ne!(block, s.graph.block_at[&1]);
}
#[test]
fn parallel_normal_and_exceptional_edges_merge_old_and_new() {
    // Deliberately use the same target to test edge state, independent of verifier rules.
    let c = code(&[0x1012, 0x1021, 0x000f], 2, 1);
    let ir = DecodedMethod::decode(&c).unwrap();
    let mut graph = ControlFlowGraph::build(&c).unwrap();
    // Split manually into instruction blocks with duplicate-kind target.
    use rdx::native_cfg::{BasicBlock, Edge, EdgeKind};
    graph.blocks = vec![
        BasicBlock {
            start: 0,
            end: 1,
            instructions: vec![0],
            successors: vec![Edge {
                target: 1,
                kind: EdgeKind::Normal,
            }],
        },
        BasicBlock {
            start: 1,
            end: 2,
            instructions: vec![1],
            successors: vec![
                Edge {
                    target: 2,
                    kind: EdgeKind::Normal,
                },
                Edge {
                    target: 2,
                    kind: EdgeKind::Exceptional,
                },
            ],
        },
        BasicBlock {
            start: 2,
            end: 3,
            instructions: vec![2],
            successors: vec![],
        },
    ];
    graph.block_at = [(0, 0), (1, 1), (2, 2)].into();
    let s = SsaMethod::build(&c, &ir, &graph).unwrap();
    let p = s.phis.iter().find(|p| p.block == 2).unwrap();
    assert_eq!(read(&s, 2), p.result);
    assert!(p.incoming.iter().any(|v| v.1 == write(&s, 0)));
    assert!(p.incoming.iter().any(|v| v.1 == write(&s, 1)));
}
#[test]
fn partial_wide_overwrite_preserves_distinct_word_origins() {
    let s = build(&code(&[0x0016, 7, 0x1112, 0x0010], 2, 0));
    let wide = &s.instructions.iter().find(|i| i.pc == 3).unwrap().reads[0];
    assert_eq!(wide.words[0], write(&s, 0));
    assert_eq!(wide.words[1], write(&s, 2));
    assert!(matches!(
        s.definitions[wide.words[0]].kind,
        DefinitionKind::Instruction { pc: 0, word: 0, .. }
    ));
    assert!(matches!(
        s.definitions[wide.words[1]].kind,
        DefinitionKind::Instruction { pc: 2, word: 0, .. }
    ));
}
#[test]
fn overlapping_wide_move_reads_both_words_before_writing() {
    let s = build(&code(&[0x0016, 7, 0x0104, 0x0110], 3, 0));
    let initial = &s.instructions[0].writes[0].words;
    assert_eq!(&s.instructions[1].reads[0].words, initial);
    assert_eq!(
        s.instructions[2].reads[0].words,
        s.instructions[1].writes[0].words
    );
}
#[test]
fn undefined_locals_and_unreachable_code_are_explicit() {
    let s = build(&code(&[0x0228, 0x000f, 0x010f], 2, 1));
    assert_eq!(s.instructions.len(), 2);
    assert_eq!(s.definitions[0].kind, DefinitionKind::Undefined);
    assert_eq!(s.definitions[read(&s, 2)].kind, DefinitionKind::Parameter);
    let u = build(&code(&[0x000f], 1, 0));
    assert_eq!(u.definitions[read(&u, 0)].kind, DefinitionKind::Undefined);
}
#[test]
fn work_budget_and_mismatched_ir_are_rejected() {
    let c = code(&[0x000e], 1, 0);
    let ir = DecodedMethod::decode(&c).unwrap();
    let graph = ControlFlowGraph::build(&c).unwrap();
    assert!(SsaMethod::build_with_work_limit(&c, &ir, &graph, 0).is_err());
    assert!(
        SsaMethod::build(
            &c,
            &DecodedMethod {
                instructions: vec![]
            },
            &graph
        )
        .is_err()
    );
}

#[test]
fn protected_cast_read_and_exception_use_old_receiver() {
    let mut c = code(&[0x001f, 0, 0x0011, 0x010d, 0x0011], 2, 2);
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 0,
        end: 2,
        catches: Arc::from([(None, 3)]),
    }];
    let s = build(&c);
    assert_eq!(read(&s, 0), 0);
    assert_eq!(read(&s, 4), 0);
    assert_eq!(read(&s, 2), write(&s, 0));
}
#[test]
fn many_registers_do_not_allocate_block_times_register_matrix() {
    let c = code(&[0x0013, 7, 0x000f], u16::MAX, 1);
    let s = build(&c);
    assert_eq!(s.definitions.len(), usize::from(u16::MAX) + 1);
    assert!(s.phis.is_empty());
    assert_eq!(read(&s, 2), write(&s, 0));
}

#[test]
fn nonthrowing_protected_writes_do_not_supply_exception_phi_inputs() {
    // Handler can see v0=2 from before array-length; const v0=2 cannot itself throw.
    let mut c = code(&[0x1012, 0x2012, 0x1021, 0x000f, 0x020d, 0x000f], 3, 2);
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 1,
        end: 3,
        catches: Arc::from([(Some(Arc::from("Ljava/lang/Exception;")), 4)]),
    }];
    let s = build(&c);
    assert_eq!(read(&s, 5), write(&s, 1));
    assert_ne!(read(&s, 5), write(&s, 0));
    assert_ne!(read(&s, 5), write(&s, 2));
    assert!(s.phis.is_empty());
}

#[test]
fn handler_of_entirely_nonthrowing_region_is_unreachable_in_ssa() {
    let mut c = code(&[0x1012, 0x000f, 0x010d, 0x000f], 2, 0);
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 0,
        end: 1,
        catches: Arc::from([(Some(Arc::from("Ljava/lang/Exception;")), 2)]),
    }];
    let s = build(&c);
    assert!(!s.reachable[s.graph.block_at[&2]]);
    assert!(!s.instructions.iter().any(|instruction| instruction.pc >= 2));
}
