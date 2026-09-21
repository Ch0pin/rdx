//! Dominators and dominance frontiers adapted from JADX 1.5.6
//! `jadx/core/dex/visitors/blocks/DominatorTree.java`, pinned at
//! 28ff15e4ae69950aebea110a13e5ab895d234dfc (Apache-2.0).
//!
//! Uses the same Cooper/Harvey/Kennedy iterative immediate-dominator algorithm
//! and predecessor walks for frontiers. Rust differences: iterative traversal,
//! bounded work/frontier storage, stable original block IDs, sparse frontiers,
//! and tree intervals instead of storing a quadratic set of dominators.
//! A virtual predecessor of entry handles back edges to entry. Both normal and
//! conservative exceptional CFG edges participate. Blocks unreachable from the
//! actual method entry are reported explicitly, not made artificial entry roots.

use crate::native_cfg::ControlFlowGraph;
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

const MAX_BLOCKS: usize = 250_000;
const MAX_EDGES: usize = 1_000_000;
const MAX_FRONTIER_ENTRIES: usize = 1_000_000;
const DEFAULT_WORK_LIMIT: usize = 20_000_000;

#[derive(Debug)]
pub struct DominatorTree {
    /// Real graph predecessors, including unreachable ones; deduplicated by ID.
    pub predecessors: Vec<Vec<usize>>,
    /// None for entry and unreachable blocks; consult `reachable` to distinguish.
    pub immediate_dominators: Vec<Option<usize>>,
    pub frontiers: Vec<Vec<usize>>,
    pub reachable: Vec<bool>,
    pub reverse_postorder: Vec<usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
}

struct Work(usize);
impl Work {
    fn charge(&mut self) -> Result<()> {
        ensure!(self.0 != 0, "dominator analysis exceeds work budget");
        self.0 -= 1;
        Ok(())
    }
}

impl DominatorTree {
    pub fn compute(graph: &ControlFlowGraph) -> Result<Self> {
        Self::compute_with_work_limit(graph, DEFAULT_WORK_LIMIT)
    }

    pub fn compute_with_work_limit(graph: &ControlFlowGraph, limit: usize) -> Result<Self> {
        let n = graph.blocks.len();
        ensure!(n > 0 && n <= MAX_BLOCKS, "invalid dominator block count");
        let root = n;
        let mut work = Work(limit);
        let mut predecessors = vec![Vec::new(); n + 1];
        let mut successors = vec![Vec::new(); n + 1];
        let mut edge_count = 0usize;
        for (from, block) in graph.blocks.iter().enumerate() {
            work.charge()?;
            for edge in &block.successors {
                work.charge()?;
                ensure!(edge.target < n, "dominator edge target outside graph");
                edge_count += 1;
                ensure!(
                    edge_count <= MAX_EDGES,
                    "dominator graph exceeds edge budget"
                );
                successors[from].push(edge.target);
                predecessors[edge.target].push(from);
            }
        }
        for list in predecessors.iter_mut().chain(successors.iter_mut()) {
            list.sort_unstable();
            list.dedup();
        }
        successors[root].push(0);
        predecessors[0].push(root);

        // Iterative postorder DFS avoids host stack overflow on long methods.
        let mut visited = vec![false; n + 1];
        let mut postorder = Vec::with_capacity(n + 1);
        let mut stack = vec![(root, 0usize)];
        visited[root] = true;
        while let Some((node, index)) = stack.last_mut() {
            work.charge()?;
            if let Some(&next) = successors[*node].get(*index) {
                *index += 1;
                if !visited[next] {
                    visited[next] = true;
                    stack.push((next, 0));
                }
            } else {
                postorder.push(*node);
                stack.pop();
            }
        }
        postorder.reverse();
        let mut rank = vec![usize::MAX; n + 1];
        for (index, &node) in postorder.iter().enumerate() {
            rank[node] = index;
        }
        let mut idom = vec![None; n + 1];
        idom[root] = Some(root);
        let mut changed = true;
        while changed {
            changed = false;
            for &node in &postorder[1..] {
                work.charge()?;
                let mut candidate = None;
                for &pred in &predecessors[node] {
                    work.charge()?;
                    if idom[pred].is_none() {
                        continue;
                    }
                    candidate = Some(match candidate {
                        None => pred,
                        Some(old) => intersect(old, pred, &idom, &rank, &mut work)?,
                    });
                }
                ensure!(
                    candidate.is_some(),
                    "reachable block has no known dominator"
                );
                if idom[node] != candidate {
                    idom[node] = candidate;
                    changed = true;
                }
            }
        }

        let mut frontiers = vec![BTreeSet::new(); n + 1];
        let mut entries = 0usize;
        for &node in &postorder[1..] {
            // Includes virtual entry predecessor: entry can be in its own frontier.
            if predecessors[node].iter().filter(|&&p| visited[p]).count() < 2 {
                continue;
            }
            let stop = idom[node].expect("reachable block has dominator");
            for &pred in &predecessors[node] {
                if !visited[pred] {
                    continue;
                }
                let mut runner = pred;
                while runner != stop {
                    work.charge()?;
                    if frontiers[runner].insert(node) {
                        entries += 1;
                        ensure!(
                            entries <= MAX_FRONTIER_ENTRIES,
                            "dominance frontier exceeds entry budget"
                        );
                    }
                    runner = idom[runner].expect("reachable predecessor has dominator");
                }
            }
        }

        // Tree intervals give constant-time dominance without per-block bitsets.
        let mut children = vec![Vec::new(); n + 1];
        for &node in &postorder[1..] {
            children[idom[node].expect("reachable block has dominator")].push(node);
        }
        let mut enter = vec![0; n + 1];
        let mut exit = vec![0; n + 1];
        let mut clock = 0;
        let mut stack = vec![(root, false)];
        while let Some((node, leaving)) = stack.pop() {
            work.charge()?;
            if leaving {
                exit[node] = clock;
            } else {
                enter[node] = clock;
                clock += 1;
                stack.push((node, true));
                stack.extend(children[node].iter().rev().map(|&child| (child, false)));
            }
        }
        predecessors[0].retain(|&p| p != root);
        predecessors.truncate(n);
        let immediate_dominators = idom[..n]
            .iter()
            .map(|&p| p.filter(|&p| p != root))
            .collect();
        frontiers.truncate(n);
        visited.truncate(n);
        enter.truncate(n);
        exit.truncate(n);
        Ok(Self {
            predecessors,
            immediate_dominators,
            frontiers: frontiers
                .into_iter()
                .map(|set| set.into_iter().collect())
                .collect(),
            reachable: visited,
            reverse_postorder: postorder.into_iter().filter(|&node| node != root).collect(),
            enter,
            exit,
        })
    }

    pub fn dominates(&self, dominator: usize, block: usize) -> bool {
        self.reachable.get(dominator) == Some(&true)
            && self.reachable.get(block) == Some(&true)
            && self.enter[dominator] <= self.enter[block]
            && self.enter[block] < self.exit[dominator]
    }
}

fn intersect(
    mut a: usize,
    mut b: usize,
    idom: &[Option<usize>],
    rank: &[usize],
    work: &mut Work,
) -> Result<usize> {
    while a != b {
        work.charge()?;
        if rank[a] > rank[b] {
            a = idom[a].expect("intersection only visits initialized dominators");
        } else {
            b = idom[b].expect("intersection only visits initialized dominators");
        }
    }
    Ok(a)
}
