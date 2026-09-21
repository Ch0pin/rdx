//! Register SSA adapted from JADX SSATransform (Apache-2.0), commit
//! 28ff15e4ae69950aebea110a13e5ab895d234dfc: live-in pruned iterated dominance
//! frontiers followed by dominator-tree renaming. Unlike JADX's post-renaming
//! try repair, normal-success write blocks preserve the pre-write exceptional
//! state structurally. Values are register WORDS, not inferred Java variables;
//! a wide operand keeps both words together. Partial overwrites remain visible
//! as distinct word definitions; consumers MUST establish pair/type coherence
//! before treating a pair as a Java long/double (parameters require signatures). No phi simplification or type
//! inference is performed here. Unreachable instructions are not renamed.
use crate::native_cfg::{BasicBlock, ControlFlowGraph, Edge, EdgeKind};
use crate::native_dex::DexCode;
use crate::native_dominators::DominatorTree;
use crate::native_ir::{DecodedMethod, RegisterOperand, ValueKind};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const LIMIT: usize = 1_000_000;
const WORK_LIMIT: usize = 20_000_000;
pub type ValueId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionKind {
    Parameter,
    Undefined,
    Instruction {
        pc: usize,
        word: usize,
        block: usize,
    },
    Phi {
        block: usize,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub register: u16,
    pub kind: DefinitionKind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsaOperand {
    pub register: u16,
    pub kind: ValueKind,
    pub words: Vec<ValueId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsaInstruction {
    pub pc: usize,
    pub reads: Vec<SsaOperand>,
    pub writes: Vec<SsaOperand>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phi {
    pub block: usize,
    pub register: u16,
    pub result: ValueId,
    /// None is the virtual method-entry predecessor (including undefined locals).
    pub incoming: Vec<(Option<usize>, ValueId)>,
}
#[derive(Debug)]
pub struct SsaMethod {
    pub definitions: Vec<Definition>,
    pub instructions: Vec<SsaInstruction>,
    pub phis: Vec<Phi>,
    /// Original block IDs remain stable; appended blocks commit successful writes.
    pub graph: ControlFlowGraph,
    pub reachable: Vec<bool>,
}
struct Budget {
    work: usize,
    entries: usize,
}
impl Budget {
    fn work(&mut self) -> Result<()> {
        ensure!(self.work > 0, "SSA exceeds work budget");
        self.work -= 1;
        Ok(())
    }
    fn entries(&mut self, count: usize) -> Result<()> {
        self.entries = self
            .entries
            .checked_add(count)
            .context("SSA size overflow")?;
        ensure!(self.entries <= LIMIT, "SSA exceeds storage budget");
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Step {
    instruction: usize,
    reads: bool,
    writes: bool,
}

impl SsaMethod {
    pub fn build(code: &DexCode, ir: &DecodedMethod, graph: &ControlFlowGraph) -> Result<Self> {
        Self::build_with_work_limit(code, ir, graph, WORK_LIMIT)
    }
    /// Limits SSA-local work; dominator construction has its own independent bound.
    pub fn build_with_work_limit(
        code: &DexCode,
        ir: &DecodedMethod,
        input: &ControlFlowGraph,
        work_limit: usize,
    ) -> Result<Self> {
        ensure!(
            code.ins <= code.registers,
            "SSA input words exceed registers"
        );
        ensure!(
            !input.blocks.is_empty() && input.blocks.len() <= 250_000,
            "invalid SSA block count"
        );
        ensure!(
            ir.instructions.len() <= LIMIT,
            "SSA instruction budget exceeded"
        );
        ensure!(
            code.instructions.len() <= LIMIT,
            "SSA code-unit budget exceeded"
        );
        let mut budget = Budget {
            work: work_limit,
            entries: 0,
        };
        let mut input_entries = 0usize;
        for block in &input.blocks {
            budget.work()?;
            input_entries = input_entries
                .checked_add(block.instructions.len())
                .and_then(|n| n.checked_add(block.successors.len()))
                .context("SSA input size overflow")?;
            ensure!(
                input_entries <= LIMIT,
                "SSA input graph exceeds storage budget"
            );
        }
        ensure!(
            input.block_at.len() <= LIMIT,
            "SSA input map exceeds storage budget"
        );
        let mut at = HashMap::new();
        let mut operand_words = 0usize;
        for (index, instruction) in ir.instructions.iter().enumerate() {
            budget.work()?;
            ensure!(
                at.insert(instruction.pc, index).is_none(),
                "duplicate SSA instruction offset"
            );
            for operand in instruction.reads.iter().chain(&instruction.writes) {
                budget.work()?;
                operand_words = operand_words
                    .checked_add(operand.kind.word_count())
                    .context("SSA operand size overflow")?;
                ensure!(operand_words <= LIMIT, "SSA operand budget exceeded");
                ensure!(
                    usize::from(operand.register) + operand.kind.word_count()
                        <= usize::from(code.registers),
                    "SSA operand outside registers"
                );
            }
        }
        let mut graph = input.clone();
        let mut steps = vec![Vec::new(); graph.blocks.len()];
        let mut seen = BTreeSet::new();
        for block in 0..input.blocks.len() {
            budget.work()?;
            for &pc in &input.blocks[block].instructions {
                ensure!(
                    seen.insert(pc),
                    "SSA instruction appears in multiple blocks"
                );
                let index = *at
                    .get(&pc)
                    .context("CFG instruction missing from decoded IR")?;
                steps[block].push(Step {
                    instruction: index,
                    reads: true,
                    writes: true,
                });
            }
            // The splitter conservatively attaches handlers to every protected
            // instruction. Only throwing instructions can carry exceptional SSA
            // state; otherwise register reuse introduces impossible typed phis.
            if steps[block]
                .iter()
                .all(|step| !ir.instructions[step.instruction].may_throw)
            {
                graph.blocks[block]
                    .successors
                    .retain(|edge| edge.kind == EdgeKind::Normal);
            }
            if graph.blocks[block]
                .successors
                .iter()
                .any(|edge| edge.kind == EdgeKind::Exceptional)
                && steps[block]
                    .iter()
                    .any(|step| !ir.instructions[step.instruction].writes.is_empty())
            {
                ensure!(
                    steps[block].len() == 1,
                    "SSA exceptional writing block must contain one instruction"
                );
                ensure!(
                    graph.blocks.len() < 250_000,
                    "SSA normalized block budget exceeded"
                );
                let normal = graph.blocks[block]
                    .successors
                    .iter()
                    .copied()
                    .filter(|edge| edge.kind == EdgeKind::Normal)
                    .collect();
                let synthetic = graph.blocks.len();
                graph.blocks[block]
                    .successors
                    .retain(|edge| edge.kind == EdgeKind::Exceptional);
                graph.blocks[block].successors.push(Edge {
                    target: synthetic,
                    kind: EdgeKind::Normal,
                });
                graph.blocks.push(BasicBlock {
                    start: input.blocks[block].end,
                    end: input.blocks[block].end,
                    instructions: vec![],
                    successors: normal,
                });
                steps[block][0].writes = false;
                steps.push(vec![Step {
                    instruction: steps[block][0].instruction,
                    reads: false,
                    writes: true,
                }]);
            }
        }
        let dom = DominatorTree::compute(&graph)?;
        let n = graph.blocks.len();
        let mut assigned = vec![BTreeSet::new(); n];
        let mut live = vec![BTreeSet::new(); n];
        let mut sites = BTreeMap::<u16, BTreeSet<usize>>::new();
        let mut pending = Vec::new();
        for block in 0..n {
            if !dom.reachable[block] {
                continue;
            }
            for step in &steps[block] {
                let instruction = &ir.instructions[step.instruction];
                if step.reads {
                    for operand in &instruction.reads {
                        for word in 0..operand.kind.word_count() {
                            budget.work()?;
                            let reg = operand.register + word as u16;
                            if !assigned[block].contains(&reg) && live[block].insert(reg) {
                                budget.entries(1)?;
                                pending.push((block, reg));
                            }
                        }
                    }
                }
                if step.writes {
                    for operand in &instruction.writes {
                        for word in 0..operand.kind.word_count() {
                            budget.work()?;
                            let reg = operand.register + word as u16;
                            if assigned[block].insert(reg) {
                                budget.entries(2)?;
                                sites.entry(reg).or_default().insert(block);
                            }
                        }
                    }
                }
            }
        }
        while let Some((block, reg)) = pending.pop() {
            for &pred in &dom.predecessors[block] {
                budget.work()?;
                if dom.reachable[pred] && !assigned[pred].contains(&reg) && live[pred].insert(reg) {
                    budget.entries(1)?;
                    pending.push((pred, reg));
                }
            }
        }
        let mut phi_regs = vec![BTreeSet::new(); n];
        for (reg, blocks) in sites {
            let mut visited = blocks.clone();
            let mut work: Vec<_> = blocks.into_iter().collect();
            while let Some(block) = work.pop() {
                for &frontier in &dom.frontiers[block] {
                    budget.work()?;
                    if live[frontier].contains(&reg) && phi_regs[frontier].insert(reg) {
                        budget.entries(1)?;
                        if visited.insert(frontier) {
                            work.push(frontier);
                        }
                    }
                }
            }
        }
        let mut definitions = Vec::new();
        let mut stacks = Vec::new();
        for reg in 0..code.registers {
            budget.entries(1)?;
            stacks.push(vec![definitions.len()]);
            definitions.push(Definition {
                register: reg,
                kind: if reg >= code.registers - code.ins {
                    DefinitionKind::Parameter
                } else {
                    DefinitionKind::Undefined
                },
            });
        }
        let mut phis = Vec::new();
        let mut block_phis = vec![Vec::new(); n];
        for block in 0..n {
            for &reg in &phi_regs[block] {
                budget.entries(2)?;
                let result = definitions.len();
                definitions.push(Definition {
                    register: reg,
                    kind: DefinitionKind::Phi { block },
                });
                block_phis[block].push(phis.len());
                phis.push(Phi {
                    block,
                    register: reg,
                    result,
                    incoming: if block == 0 {
                        vec![(None, usize::from(reg))]
                    } else {
                        vec![]
                    },
                });
            }
        }
        let mut children = vec![Vec::new(); n];
        for (block, parent) in dom.immediate_dominators.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(block);
            }
        }
        let mut instructions = BTreeMap::<usize, SsaInstruction>::new();
        enum Visit {
            Enter(usize),
            Exit(Vec<u16>),
        }
        let mut visits = vec![Visit::Enter(0)];
        while let Some(visit) = visits.pop() {
            budget.work()?;
            match visit {
                Visit::Exit(registers) => {
                    for reg in registers {
                        stacks[usize::from(reg)].pop();
                    }
                }
                Visit::Enter(block) => {
                    let mut changed = Vec::new();
                    for &index in &block_phis[block] {
                        let phi = &phis[index];
                        stacks[usize::from(phi.register)].push(phi.result);
                        changed.push(phi.register);
                    }
                    for step in &steps[block] {
                        let instruction = &ir.instructions[step.instruction];
                        let output =
                            instructions
                                .entry(instruction.pc)
                                .or_insert_with(|| SsaInstruction {
                                    pc: instruction.pc,
                                    reads: vec![],
                                    writes: vec![],
                                });
                        if step.reads {
                            for operand in &instruction.reads {
                                budget.work()?;
                                budget.entries(operand.kind.word_count())?;
                                output.reads.push(read_operand(operand, &stacks));
                            }
                        }
                        if step.writes {
                            for operand in &instruction.writes {
                                let mut words = Vec::new();
                                for word in 0..operand.kind.word_count() {
                                    budget.work()?;
                                    budget.entries(2)?;
                                    let reg = operand.register + word as u16;
                                    let id = definitions.len();
                                    definitions.push(Definition {
                                        register: reg,
                                        kind: DefinitionKind::Instruction {
                                            pc: instruction.pc,
                                            word,
                                            block,
                                        },
                                    });
                                    stacks[usize::from(reg)].push(id);
                                    changed.push(reg);
                                    words.push(id);
                                }
                                output.writes.push(SsaOperand {
                                    register: operand.register,
                                    kind: operand.kind,
                                    words,
                                });
                            }
                        }
                    }
                    let successors: BTreeSet<_> = graph.blocks[block]
                        .successors
                        .iter()
                        .map(|edge| edge.target)
                        .collect();
                    for successor in successors {
                        for &index in &block_phis[successor] {
                            budget.work()?;
                            budget.entries(1)?;
                            let phi = &mut phis[index];
                            phi.incoming.push((
                                Some(block),
                                *stacks[usize::from(phi.register)].last().unwrap(),
                            ));
                        }
                    }
                    visits.push(Visit::Exit(changed));
                    for &child in children[block].iter().rev() {
                        visits.push(Visit::Enter(child));
                    }
                }
            }
        }
        for phi in &mut phis {
            phi.incoming.sort_unstable_by_key(|pair| pair.0);
        }
        Ok(Self {
            definitions,
            instructions: instructions.into_values().collect(),
            phis,
            graph,
            reachable: dom.reachable,
        })
    }
}
fn read_operand(operand: &RegisterOperand, stacks: &[Vec<ValueId>]) -> SsaOperand {
    SsaOperand {
        register: operand.register,
        kind: operand.kind,
        words: (0..operand.kind.word_count())
            .map(|word| *stacks[usize::from(operand.register) + word].last().unwrap())
            .collect(),
    }
}
