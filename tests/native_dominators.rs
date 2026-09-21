use rdx::{
    native_cfg::{BasicBlock, ControlFlowGraph, Edge, EdgeKind},
    native_dominators::DominatorTree,
};
use std::collections::{BTreeSet, HashMap};

fn graph(successors: Vec<Vec<(usize, EdgeKind)>>) -> ControlFlowGraph {
    let blocks = successors
        .into_iter()
        .enumerate()
        .map(|(index, edges)| BasicBlock {
            start: index,
            end: index + 1,
            instructions: vec![index],
            successors: edges
                .into_iter()
                .map(|(target, kind)| Edge { target, kind })
                .collect(),
        })
        .collect::<Vec<_>>();
    ControlFlowGraph {
        block_at: (0..blocks.len())
            .map(|index| (index, index))
            .collect::<HashMap<_, _>>(),
        blocks,
    }
}

#[derive(Debug)]
struct Oracle {
    predecessors: Vec<Vec<usize>>,
    reachable: Vec<bool>,
    dominators: Vec<BTreeSet<usize>>,
    idoms: Vec<Option<usize>>,
    frontiers: Vec<Vec<usize>>,
}

fn oracle(cfg: &ControlFlowGraph) -> Oracle {
    let count = cfg.blocks.len();
    let mut predecessors = vec![BTreeSet::new(); count];
    for (source, block) in cfg.blocks.iter().enumerate() {
        for edge in &block.successors {
            assert!(edge.target < count, "oracle requires valid targets");
            predecessors[edge.target].insert(source);
        }
    }

    let mut reachable = vec![false; count];
    if count != 0 {
        let mut stack = vec![0];
        while let Some(node) = stack.pop() {
            if reachable[node] {
                continue;
            }
            reachable[node] = true;
            stack.extend(cfg.blocks[node].successors.iter().map(|edge| edge.target));
        }
    }

    let universe = (0..count)
        .filter(|&node| reachable[node])
        .collect::<BTreeSet<_>>();
    let mut dominators = vec![BTreeSet::new(); count];
    for node in 0..count {
        if reachable[node] {
            dominators[node] = if node == 0 {
                BTreeSet::from([0])
            } else {
                universe.clone()
            };
        }
    }
    loop {
        let mut changed = false;
        for node in 1..count {
            if !reachable[node] {
                continue;
            }
            let mut incoming = predecessors[node].iter().copied().filter(|&p| reachable[p]);
            let mut next = incoming
                .next()
                .map(|first| dominators[first].clone())
                .unwrap_or_default();
            for pred in incoming {
                next = next.intersection(&dominators[pred]).copied().collect();
            }
            next.insert(node);
            if next != dominators[node] {
                dominators[node] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut idoms = vec![None; count];
    for node in 1..count {
        if !reachable[node] {
            continue;
        }
        let strict = dominators[node]
            .iter()
            .copied()
            .filter(|&candidate| candidate != node)
            .collect::<Vec<_>>();
        idoms[node] = strict.iter().copied().find(|candidate| {
            strict
                .iter()
                .all(|other| other == candidate || dominators[*candidate].contains(other))
        });
    }

    let mut frontiers = vec![Vec::new(); count];
    for x in 0..count {
        if !reachable[x] {
            continue;
        }
        for y in 0..count {
            if !reachable[y] || (x != y && dominators[y].contains(&x)) {
                continue;
            }
            if predecessors[y]
                .iter()
                .copied()
                .filter(|&pred| reachable[pred])
                .any(|pred| dominators[pred].contains(&x))
            {
                frontiers[x].push(y);
            }
        }
    }

    Oracle {
        predecessors: predecessors
            .into_iter()
            .map(|set| set.into_iter().collect())
            .collect(),
        reachable,
        dominators,
        idoms,
        frontiers,
    }
}

fn assert_matches_oracle(cfg: &ControlFlowGraph) {
    let expected = oracle(cfg);
    let actual = DominatorTree::compute(cfg).unwrap();
    assert_eq!(actual.reachable, expected.reachable);
    assert_eq!(actual.immediate_dominators, expected.idoms);

    for node in 0..cfg.blocks.len() {
        let mut actual_preds = actual.predecessors[node].clone();
        actual_preds.sort_unstable();
        actual_preds.dedup();
        assert_eq!(
            actual_preds, expected.predecessors[node],
            "predecessors of {node}"
        );

        let mut actual_frontier = actual.frontiers[node].clone();
        actual_frontier.sort_unstable();
        actual_frontier.dedup();
        assert_eq!(
            actual_frontier, expected.frontiers[node],
            "frontier of {node}"
        );
        for other in 0..cfg.blocks.len() {
            assert_eq!(
                actual.dominates(node, other),
                expected.reachable[other] && expected.dominators[other].contains(&node),
                "dominates({node}, {other})"
            );
        }
    }

    let rpo = actual
        .reverse_postorder
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let reachable = expected
        .reachable
        .iter()
        .enumerate()
        .filter_map(|(node, yes)| yes.then_some(node))
        .collect::<BTreeSet<_>>();
    assert_eq!(rpo, reachable);
    assert_eq!(actual.reverse_postorder.first(), reachable.first());
}

#[test]
fn exhaustive_graphs_through_three_nodes_match_set_oracle() {
    for count in 1..=3 {
        for mask in 0usize..(1usize << (count * count)) {
            let mut edges = vec![Vec::new(); count];
            for (source, successors) in edges.iter_mut().enumerate() {
                for target in 0..count {
                    if mask & (1 << (source * count + target)) != 0 {
                        let kind =
                            if (source + target + mask.count_ones() as usize).is_multiple_of(2) {
                                EdgeKind::Normal
                            } else {
                                EdgeKind::Exceptional
                            };
                        successors.push((target, kind));
                    }
                }
            }
            assert_matches_oracle(&graph(edges));
        }
    }
}

#[test]
fn seeded_larger_graphs_match_set_oracle() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for count in 4..=10 {
        for _ in 0..40 {
            let mut edges = vec![Vec::new(); count];
            for successors in &mut edges {
                for target in 0..count {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    if state.is_multiple_of(5) {
                        let kind = if state & 1 == 0 {
                            EdgeKind::Normal
                        } else {
                            EdgeKind::Exceptional
                        };
                        successors.push((target, kind));
                    }
                }
            }
            assert_matches_oracle(&graph(edges));
        }
    }
}

#[test]
fn fixed_diamonds_loops_irreducible_and_exceptional_edges_match_oracle() {
    let cases = [
        graph(vec![
            vec![(1, EdgeKind::Normal), (2, EdgeKind::Normal)],
            vec![(3, EdgeKind::Normal)],
            vec![(3, EdgeKind::Normal)],
            vec![],
        ]),
        graph(vec![
            vec![(1, EdgeKind::Normal)],
            vec![(2, EdgeKind::Normal)],
            vec![(1, EdgeKind::Normal), (3, EdgeKind::Normal)],
            vec![],
        ]),
        graph(vec![
            vec![(1, EdgeKind::Normal), (2, EdgeKind::Normal)],
            vec![(2, EdgeKind::Normal)],
            vec![(1, EdgeKind::Normal), (3, EdgeKind::Normal)],
            vec![],
        ]),
        graph(vec![
            vec![(1, EdgeKind::Normal), (2, EdgeKind::Exceptional)],
            vec![(3, EdgeKind::Exceptional)],
            vec![(3, EdgeKind::Normal)],
            vec![],
        ]),
    ];
    for cfg in cases {
        assert_matches_oracle(&cfg);
    }
}

#[test]
fn disconnected_handler_and_self_looping_entry_obey_entry_only_reachability() {
    let cfg = graph(vec![
        vec![(0, EdgeKind::Normal), (1, EdgeKind::Normal)],
        vec![],
        vec![(2, EdgeKind::Exceptional)],
    ]);
    let tree = DominatorTree::compute(&cfg).unwrap();
    assert_matches_oracle(&cfg);
    assert_eq!(tree.reachable, vec![true, true, false]);
    assert_eq!(tree.immediate_dominators, vec![None, Some(0), None]);
    assert!(tree.frontiers[2].is_empty());
    assert!(tree.frontiers[0].contains(&0));
}

#[test]
fn malformed_graphs_and_work_budget_are_rejected() {
    assert!(DominatorTree::compute(&graph(vec![])).is_err());
    let malformed = graph(vec![vec![(2, EdgeKind::Normal)], vec![]]);
    assert!(DominatorTree::compute(&malformed).is_err());

    let cfg = graph(vec![
        vec![(1, EdgeKind::Normal)],
        vec![(2, EdgeKind::Normal)],
        vec![],
    ]);
    assert!(DominatorTree::compute_with_work_limit(&cfg, 0).is_err());
    let bounded = DominatorTree::compute_with_work_limit(&cfg, 10_000).unwrap();
    let regular = DominatorTree::compute(&cfg).unwrap();
    assert_eq!(bounded.immediate_dominators, regular.immediate_dominators);
    assert_eq!(bounded.frontiers, regular.frontiers);
}

#[test]
fn long_linear_graph_is_iterative_and_keeps_frontiers_empty() {
    const COUNT: usize = 20_000;
    let mut edges = vec![Vec::new(); COUNT];
    for (node, successors) in edges.iter_mut().enumerate().take(COUNT - 1) {
        successors.push((node + 1, EdgeKind::Normal));
    }

    let tree = DominatorTree::compute(&graph(edges)).unwrap();
    let middle = COUNT / 2;
    assert_eq!(tree.reverse_postorder.len(), COUNT);
    assert!(tree.reachable.iter().all(|reachable| *reachable));
    assert_eq!(tree.immediate_dominators[1], Some(0));
    assert_eq!(tree.immediate_dominators[middle], Some(middle - 1));
    assert_eq!(tree.immediate_dominators[COUNT - 1], Some(COUNT - 2));
    assert!(tree.dominates(0, COUNT - 1));
    assert!(tree.dominates(middle, COUNT - 1));
    assert!(!tree.dominates(COUNT - 1, middle));
    assert!(tree.frontiers.iter().all(Vec::is_empty));
}

#[test]
fn duplicate_edge_kinds_are_one_predecessor_and_queries_are_bounds_checked() {
    let cfg = graph(vec![
        vec![(1, EdgeKind::Normal), (1, EdgeKind::Exceptional)],
        vec![(2, EdgeKind::Normal)],
        vec![],
    ]);
    let tree = DominatorTree::compute(&cfg).unwrap();
    assert_matches_oracle(&cfg);
    assert_eq!(tree.predecessors[1], vec![0]);
    assert!(!tree.dominates(3, 0));
    assert!(!tree.dominates(0, 3));
    assert!(!tree.dominates(usize::MAX, usize::MAX));
}
