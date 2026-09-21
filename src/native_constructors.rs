//! Constructor receiver identity analysis adapted from JADX ConstructorVisitor
//! (Apache-2.0), commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Follows SSA assignment chains and merges, but deliberately does not perform
//! JADX's removal/replacement of allocation instructions. A resolved origin is
//! NOT permission to move allocation, effects, or exception boundaries; this
//! pass checks allocation dominance but does not establish initialization validity.
use crate::native_call_values::SsaCalls;
use crate::native_calls::{BoundCalls, CallKind, CallTarget};
use crate::native_dex::{DexClass, DexMethod};
use crate::native_dominators::DominatorTree;
use crate::native_ir::{DecodedMethod, PoolKind};
use crate::native_ssa::{DefinitionKind, SsaMethod, ValueId};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

const LIMIT: usize = 1_000_000;
const WORK: usize = 20_000_000;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstructorOrigin {
    Allocation {
        pc: usize,
        type_descriptor: Arc<str>,
    },
    This,
    Super,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstructorBinding {
    pub invoke_pc: usize,
    pub receiver_value: ValueId,
    /// Original DEX invocation owner, never rewritten or discarded by this pass.
    pub invoked_owner: Arc<str>,
    /// The original owner differs from the allocated class or the direct
    /// superclass of a chaining call. A later emission pass must decide how to
    /// represent that difference without changing initialization behavior.
    pub owner_retarget_required: bool,
    pub origin: ConstructorOrigin,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstructorIssue {
    pub invoke_pc: usize,
    pub reason: &'static str,
}
#[derive(Debug, Default)]
pub struct ConstructorAnalysis {
    pub bindings: Vec<ConstructorBinding>,
    pub unresolved: Vec<ConstructorIssue>,
    pub unreachable_constructors: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Bottom,
    Allocation(usize),
    This,
    Unknown,
    Ambiguous,
}
fn merge(a: Origin, b: Origin) -> Origin {
    if a == Origin::Bottom {
        b
    } else if b == Origin::Bottom || a == b {
        a
    } else if a == Origin::Unknown || b == Origin::Unknown {
        Origin::Unknown
    } else {
        Origin::Ambiguous
    }
}
fn spend(work: &mut usize) -> Result<()> {
    ensure!(*work > 0, "constructor identity work budget exceeded");
    *work -= 1;
    Ok(())
}
impl ConstructorAnalysis {
    pub fn analyze(
        class: &DexClass,
        method: &DexMethod,
        ir: &DecodedMethod,
        bound: &BoundCalls,
        ssa: &SsaMethod,
        calls: &SsaCalls,
    ) -> Result<Self> {
        Self::analyze_with_work_limit(class, method, ir, bound, ssa, calls, WORK)
    }
    pub fn analyze_with_work_limit(
        class: &DexClass,
        method: &DexMethod,
        ir: &DecodedMethod,
        bound: &BoundCalls,
        ssa: &SsaMethod,
        calls: &SsaCalls,
        mut work: usize,
    ) -> Result<Self> {
        let code = method
            .code
            .as_ref()
            .context("constructor analysis requires code")?;
        ensure!(
            ssa.definitions.len() <= LIMIT
                && ir.instructions.len() <= LIMIT
                && bound.calls.len() <= LIMIT
                && calls.calls.len() <= LIMIT
                && ssa.instructions.len() <= LIMIT
                && ssa.phis.len() <= LIMIT,
            "constructor identity storage budget exceeded"
        );
        ensure!(
            code.ins <= code.registers,
            "invalid constructor parameter registers"
        );
        let mut has_constructor = false;
        for call in &bound.calls {
            spend(&mut work)?;
            if matches!(&call.target, CallTarget::Method { name, .. } if name.as_ref() == "<init>")
            {
                has_constructor = true;
                break;
            }
        }
        if !has_constructor {
            return Ok(Self::default());
        }
        let this_reg =
            (method.access_flags & 8 == 0 && code.ins > 0).then_some(code.registers - code.ins);
        let decoded: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let instructions: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
        let n = ssa.definitions.len();
        let mut states = vec![Origin::Bottom; n];
        let mut dependents = vec![Vec::new(); n];
        let mut edges = 0usize;
        for (value, definition) in ssa.definitions.iter().enumerate() {
            spend(&mut work)?;
            let mut dependencies = Vec::new();
            states[value] = match definition.kind {
                DefinitionKind::Parameter if Some(definition.register) == this_reg => Origin::This,
                DefinitionKind::Parameter | DefinitionKind::Undefined => Origin::Unknown,
                DefinitionKind::Instruction { pc, word, .. } => {
                    let ins = decoded
                        .get(&pc)
                        .context("constructor definition missing IR")?;
                    if ins.opcode == 0x22 && word == 0 {
                        Origin::Allocation(pc)
                    } else if matches!(ins.opcode, 0x07..=0x09) && word == 0 {
                        let assigned = instructions
                            .get(&pc)
                            .context("constructor move missing SSA")?;
                        ensure!(
                            assigned.reads.len() == 1 && assigned.reads[0].words.len() == 1,
                            "invalid constructor object move"
                        );
                        dependencies.push(assigned.reads[0].words[0]);
                        Origin::Bottom
                    } else {
                        Origin::Unknown
                    }
                }
                DefinitionKind::Phi { .. } => {
                    let phi = phis.get(&value).context("constructor phi missing")?;
                    ensure!(
                        phi.incoming.len() <= LIMIT - edges,
                        "constructor identity edge budget exceeded"
                    );
                    dependencies.extend(phi.incoming.iter().map(|(_, v)| *v));
                    Origin::Bottom
                }
            };
            for dependency in dependencies {
                spend(&mut work)?;
                edges += 1;
                ensure!(edges <= LIMIT, "constructor identity edge budget exceeded");
                dependents
                    .get_mut(dependency)
                    .context("constructor value outside SSA")?
                    .push(value);
            }
        }
        let mut queue: VecDeque<_> = (0..n).filter(|&v| states[v] != Origin::Bottom).collect();
        let mut queued: Vec<_> = states.iter().map(|s| *s != Origin::Bottom).collect();
        // Propagate every incoming origin. Cyclic phis with one allocation seed
        // converge to that identity; unseeded cycles are explicitly unknown.
        for phase in 0..2 {
            while let Some(value) = queue.pop_front() {
                spend(&mut work)?;
                queued[value] = false;
                for &dependent in &dependents[value] {
                    spend(&mut work)?;
                    let merged = merge(states[dependent], states[value]);
                    if merged != states[dependent] {
                        states[dependent] = merged;
                        if !queued[dependent] {
                            queue.push_back(dependent);
                            queued[dependent] = true;
                        }
                    }
                }
            }
            if phase == 0 {
                for value in 0..n {
                    spend(&mut work)?;
                    if states[value] == Origin::Bottom {
                        states[value] = Origin::Unknown;
                        queue.push_back(value);
                        queued[value] = true;
                    }
                }
            }
        }
        let dominators = DominatorTree::compute(&ssa.graph)?;
        let mut allocation_blocks = HashMap::new();
        let mut call_blocks = HashMap::new();
        for definition in &ssa.definitions {
            spend(&mut work)?;
            if let DefinitionKind::Instruction { pc, word: 0, block } = definition.kind
                && decoded.get(&pc).is_some_and(|i| i.opcode == 0x22)
            {
                allocation_blocks.insert(pc, block);
            }
        }
        for (block, data) in ssa.graph.blocks.iter().enumerate() {
            spend(&mut work)?;
            for &pc in &data.instructions {
                spend(&mut work)?;
                call_blocks.insert(pc, block);
            }
        }
        let values: HashMap<_, _> = calls.calls.iter().map(|c| (c.pc, c)).collect();
        let mut output = Self::default();
        for call in &bound.calls {
            spend(&mut work)?;
            let CallTarget::Method {
                declaring_type,
                name,
                ..
            } = &call.target
            else {
                continue;
            };
            if name.as_ref() != "<init>" {
                continue;
            }
            let Some(call_values) = values.get(&call.pc) else {
                output.unreachable_constructors += 1;
                continue;
            };
            let resolved =
                (|| -> std::result::Result<(ValueId, ConstructorOrigin), &'static str> {
                    if call.kind != CallKind::Direct || call.return_type.as_ref() != "V" {
                        return Err("constructor requires direct void invocation");
                    }
                    let receiver = call_values
                        .receiver
                        .as_ref()
                        .ok_or("constructor receiver missing")?;
                    if receiver.words.len() != 1 {
                        return Err("constructor receiver must have one SSA word");
                    }
                    let value = receiver.words[0];
                    let origin = match states
                        .get(value)
                        .ok_or("constructor receiver outside SSA")?
                    {
                        Origin::Allocation(pc) => {
                            let allocation_block = *allocation_blocks
                                .get(pc)
                                .ok_or("constructor allocation definition missing")?;
                            let call_block = *call_blocks
                                .get(&call.pc)
                                .ok_or("constructor invocation block missing")?;
                            if !dominators.dominates(allocation_block, call_block)
                                || (allocation_block == call_block && *pc >= call.pc)
                            {
                                return Err("constructor allocation does not dominate invocation");
                            }
                            let ins = decoded.get(pc).ok_or("constructor allocation missing IR")?;
                            let reference = ins
                                .reference
                                .as_ref()
                                .ok_or("constructor allocation type missing")?;
                            if reference.kind != PoolKind::Type {
                                return Err("constructor allocation requires type reference");
                            }
                            let descriptor = class
                                .symbols
                                .types
                                .get(reference.index as usize)
                                .ok_or("constructor allocation type outside pool")?;
                            if !descriptor.starts_with('L') || !descriptor.ends_with(';') {
                                return Err("constructor allocation is not a class type");
                            }
                            if descriptor != declaring_type {
                                match class.symbols.hierarchy.get().map(|hierarchy| {
                                    hierarchy.assignable(descriptor, declaring_type)
                                }) {
                                    Some(crate::native_hierarchy::Relation::Proven) => {}
                                    Some(crate::native_hierarchy::Relation::Disproven) => {
                                        return Err(
                                            "constructor owner incompatible with allocation type",
                                        );
                                    }
                                    _ => {
                                        return Err(
                                            "constructor owner relationship to allocation type is unknown",
                                        );
                                    }
                                }
                            }
                            ConstructorOrigin::Allocation {
                                pc: *pc,
                                type_descriptor: descriptor.clone(),
                            }
                        }
                        Origin::This => {
                            if method.name.as_ref() != "<init>" {
                                return Err("constructor chaining outside instance initializer");
                            }
                            if declaring_type == &class.descriptor {
                                ConstructorOrigin::This
                            } else if class.superclass.as_ref() == Some(declaring_type) {
                                ConstructorOrigin::Super
                            } else {
                                // JADX ConstructorInsn classifies every non-this
                                // owner on the this receiver as SUPER. Require a
                                // proven relationship before recording that form.
                                match class.symbols.hierarchy.get().map(|hierarchy| {
                                    hierarchy.assignable(&class.descriptor, declaring_type)
                                }) {
                                    Some(crate::native_hierarchy::Relation::Proven) => {
                                        ConstructorOrigin::Super
                                    }
                                    Some(crate::native_hierarchy::Relation::Disproven) => {
                                        return Err(
                                            "chained constructor owner incompatible with this class",
                                        );
                                    }
                                    _ => {
                                        return Err(
                                            "chained constructor owner relationship to this class is unknown",
                                        );
                                    }
                                }
                            }
                        }
                        Origin::Ambiguous => {
                            return Err(
                                "constructor receiver merges distinct allocation identities",
                            );
                        }
                        Origin::Unknown | Origin::Bottom => {
                            return Err("constructor receiver has unsupported or undefined origin");
                        }
                    };
                    Ok((value, origin))
                })();
            match resolved {
                Ok((receiver_value, origin)) => output.bindings.push(ConstructorBinding {
                    invoke_pc: call.pc,
                    receiver_value,
                    invoked_owner: declaring_type.clone(),
                    owner_retarget_required: match &origin {
                        ConstructorOrigin::Allocation {
                            type_descriptor, ..
                        } => type_descriptor != declaring_type,
                        ConstructorOrigin::Super => {
                            class.superclass.as_ref() != Some(declaring_type)
                        }
                        ConstructorOrigin::This => false,
                    },
                    origin,
                }),
                Err(reason) => output.unresolved.push(ConstructorIssue {
                    invoke_pc: call.pc,
                    reason,
                }),
            }
        }
        Ok(output)
    }
}
