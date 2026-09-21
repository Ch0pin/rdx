use rdx::{
    native_cfg::{self, EdgeKind},
    native_dex::{DexCode, DexTryRegion},
};
use std::sync::Arc;

fn code(instructions: Vec<u16>) -> DexCode {
    DexCode {
        registers: 4,
        ins: 0,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        instructions,
        offset: 0,
    }
}

fn starts(graph: &native_cfg::ControlFlowGraph, pc: usize, kind: EdgeKind) -> Vec<usize> {
    let block = graph.block_at[&pc];
    graph.blocks[block]
        .successors
        .iter()
        .filter(|edge| edge.kind == kind)
        .map(|edge| graph.blocks[edge.target].start)
        .collect()
}

#[test]
fn diamond_has_taken_and_fallthrough_edges() {
    // if-eqz v0, +4; const/4; goto +2; const/4; return
    let graph = native_cfg::build(&code(vec![0x0038, 4, 0x0112, 0x0228, 0x0212, 0x000e])).unwrap();
    assert_eq!(starts(&graph, 0, EdgeKind::Normal), vec![4, 2]);
    assert_eq!(graph.blocks[graph.block_at[&2]].instructions, vec![2, 3]);
    assert_eq!(starts(&graph, 2, EdgeKind::Normal), vec![5]);
    assert_eq!(starts(&graph, 4, EdgeKind::Normal), vec![5]);
}

#[test]
fn backward_conditional_forms_a_loop() {
    let graph = native_cfg::build(&code(vec![0x0012, 0x0038, 0xffff, 0x000e])).unwrap();
    assert_eq!(starts(&graph, 1, EdgeKind::Normal), vec![0, 3]);
}

#[test]
fn switch_excludes_payload_and_connects_cases_and_default() {
    // switch +6, default return@3, case return@4/@5, packed payload at 6.
    let graph = native_cfg::build(&code(vec![
        0x002b, 6, 0, 0x000e, 0x000e, 0x000e, 0x0100, 2, 10, 0, 4, 0, 5, 0,
    ]))
    .unwrap();
    assert_eq!(starts(&graph, 0, EdgeKind::Normal), vec![4, 5, 3]);
    assert!(!graph.block_at.contains_key(&6));
    assert!(
        graph
            .blocks
            .iter()
            .all(|block| block.instructions.iter().all(|pc| *pc < 6))
    );
}

#[test]
fn sparse_switch_connects_all_cases_without_payload_blocks() {
    // sparse-switch +6; default@3; cases -7 -> @4 and 99 -> @5.
    let graph = native_cfg::build(&code(vec![
        0x002c, 6, 0, 0x000e, 0x000e, 0x000e, 0x0200, 2, 0xfff9, 0xffff, 99, 0, 4, 0, 5, 0,
    ]))
    .unwrap();
    assert_eq!(starts(&graph, 0, EdgeKind::Normal), vec![4, 5, 3]);
    assert!(!graph.block_at.contains_key(&6));
}

#[test]
fn payload_references_require_the_exact_payload_start() {
    // The switch points at a forged packed header inside an array payload.
    let forged = code(vec![
        0x002b, 10, 0, 0x000e, 0x000e, 0x000e, 0x0300, 2, 4, 0, 0x0100, 0, 0, 0,
    ]);
    assert!(
        native_cfg::build(&forged)
            .unwrap_err()
            .to_string()
            .contains("payload boundary")
    );

    let valid_array = code(vec![0x0026, 4, 0, 0x000e, 0x0300, 1, 2, 0, 0x0201]);
    native_cfg::build(&valid_array).unwrap();
    let forged_array = code(vec![0x0026, 8, 0, 0x000e, 0x0300, 2, 4, 0, 0x0300, 1, 0, 0]);
    assert!(
        native_cfg::build(&forged_array)
            .unwrap_err()
            .to_string()
            .contains("array payload boundary")
    );
    let bad_width = code(vec![0x000e, 0, 0x0300, 3, 1, 0, 0]);
    assert!(
        native_cfg::build(&bad_width)
            .unwrap_err()
            .to_string()
            .contains("element width")
    );
}

#[test]
fn unreachable_payload_padding_is_ignored_but_reachable_fallthrough_is_rejected() {
    let graph = native_cfg::build(&code(vec![0x000e, 0, 0x0100, 0, 0, 0])).unwrap();
    assert!(!graph.block_at.contains_key(&1));

    let reachable = code(vec![0, 0, 0x0100, 0, 0, 0]);
    assert!(
        native_cfg::build(&reachable)
            .unwrap_err()
            .to_string()
            .contains("reachable normal control flow")
    );
}

#[test]
fn rejects_target_in_instruction_or_payload() {
    let interior = code(vec![0x0229, 0x0001, 0x000e]);
    assert!(
        native_cfg::build(&interior)
            .unwrap_err()
            .to_string()
            .contains("instruction boundary")
    );
    let payload = code(vec![0x0428, 0, 0, 0, 0x0100, 0, 0, 0]);
    assert!(
        native_cfg::build(&payload)
            .unwrap_err()
            .to_string()
            .contains("instruction boundary")
    );
}

#[test]
fn protected_instructions_are_split_and_receive_handler_edges() {
    let mut input = code(vec![0x0012, 0x0112, 0x0228, 0x000d, 0x000e]);
    input.tries = 1;
    input.try_regions.push(DexTryRegion {
        start: 0,
        end: 2,
        catches: Arc::from([(Some(Arc::from("Ljava/lang/Exception;")), 3)]),
    });
    let graph = native_cfg::build(&input).unwrap();
    assert_eq!(graph.blocks[graph.block_at[&0]].instructions, vec![0]);
    assert_eq!(graph.blocks[graph.block_at[&1]].instructions, vec![1]);
    assert_eq!(starts(&graph, 0, EdgeKind::Exceptional), vec![3]);
    assert_eq!(starts(&graph, 1, EdgeKind::Exceptional), vec![3]);
}

#[test]
fn malformed_try_ranges_and_handlers_fail_closed() {
    let mut input = code(vec![0x0013, 7, 0x000e]);
    input.tries = 1;
    input.try_regions.push(DexTryRegion {
        start: 1,
        end: 2,
        catches: Arc::from([(None, 2)]),
    });
    assert!(
        native_cfg::build(&input)
            .unwrap_err()
            .to_string()
            .contains("try range")
    );
    input.try_regions[0] = DexTryRegion {
        start: 0,
        end: 2,
        catches: Arc::from([(None, 1)]),
    };
    assert!(
        native_cfg::build(&input)
            .unwrap_err()
            .to_string()
            .contains("handler")
    );
}

#[test]
fn tries_count_must_match_decoded_metadata() {
    let mut input = code(vec![0x000e]);
    input.tries = 1;
    assert!(
        native_cfg::build(&input)
            .unwrap_err()
            .to_string()
            .contains("tries count")
    );
}

#[test]
fn reserved_and_truncated_instructions_fail_explicitly() {
    let reserved = code(vec![0x00e3, 0x000e]);
    assert!(
        native_cfg::build(&reserved)
            .unwrap_err()
            .to_string()
            .contains("unsupported or reserved")
    );
    let truncated_instruction = code(vec![0x0018]);
    assert!(
        native_cfg::build(&truncated_instruction)
            .unwrap_err()
            .to_string()
            .contains("truncated instruction")
    );
    let truncated_payload = code(vec![0x000e, 0, 0x0100, 2]);
    assert!(
        native_cfg::build(&truncated_payload)
            .unwrap_err()
            .to_string()
            .contains("truncated instruction or payload")
    );
}
