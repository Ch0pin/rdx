//! Adjacent catches with equal constant liveouts share one private rendering view.
//! Original instructions, exception metadata, and source offsets remain owned by DEX.
use crate::{
    native_cfg::EdgeKind,
    native_dex::{DexClass, DexMethod, DexTryRegion},
    native_method::MethodAnalysis,
    native_types::{AssignmentBound, InferredTypes},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstantCopy {
    pub pc: usize,
    pub register: u16,
    pub literal: i64,
    pub width: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub region_indices: (usize, usize),
    pub start: usize,
    pub end: usize,
    pub selected_handler: usize,
    pub caught_register: u16,
    pub common: usize,
    pub retired_prefix: std::ops::Range<usize>,
    pub constant_copies: Vec<ConstantCopy>,
    pub words: Vec<u16>,
    pub regions: Vec<DexTryRegion>,
    pub frame: (u16, u16, u16, u16),
}

struct Handler {
    entry: usize,
    caught: u16,
    copies: Vec<usize>,
    last: usize,
    common: usize,
}

fn constant(types: &InferredTypes, id: usize) -> Option<i64> {
    let value = types.values.get(id)?;
    if value.bounds_truncated || value.assignment_bounds_truncated || value.assignment.is_empty() {
        return None;
    }
    let mut found = None;
    for bound in &value.assignment {
        let AssignmentBound::Literal { bits, wide: false } = bound else {
            return None;
        };
        if found.is_some_and(|previous| previous != *bits) {
            return None;
        }
        found = Some(*bits);
    }
    found
}

pub(crate) fn validate(class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
    prove(class, method).as_ref() == Some(plan)
}

#[cfg(test)]
#[allow(dead_code)] // Source-included integration fixtures exercise the fresh route.
pub(crate) fn adjust(class: &DexClass, method: &DexMethod) -> Option<DexMethod> {
    adjust_with_retired_prefix(class, method).map(|(method, _)| method)
}

pub(crate) fn adjust_with_retired_prefix(
    class: &DexClass,
    method: &DexMethod,
) -> Option<(DexMethod, std::ops::Range<usize>)> {
    let plan = prove(class, method)?;
    debug_assert!(validate(class, method, &plan));
    Some((apply_proven(method, &plan)?, plan.retired_prefix))
}

#[cfg(test)]
#[allow(dead_code)] // Source-included integration fixtures exercise stale-plan rejection.
pub(crate) fn apply(class: &DexClass, method: &DexMethod, plan: &Plan) -> Option<DexMethod> {
    if !validate(class, method, plan) {
        return None;
    }
    apply_proven(method, plan)
}

fn apply_proven(method: &DexMethod, plan: &Plan) -> Option<DexMethod> {
    let code = method.code.as_ref()?;
    let mut instructions = code.instructions.clone();
    // This prefix has no normal ingress and no remaining exceptional owner.
    // Exact dead nops retain every following instruction's code-unit offset.
    instructions[plan.retired_prefix.clone()].fill(0);
    for copy in &plan.constant_copies {
        let dst = copy.register;
        let value = copy.literal;
        match copy.width {
            1 => instructions[copy.pc] = 0x12 | (dst << 8) | (((value as u16) & 15) << 12),
            2 => {
                instructions[copy.pc] = 0x13 | (dst << 8);
                instructions[copy.pc + 1] = value as u16;
            }
            3 => {
                instructions[copy.pc] = 0x14 | (dst << 8);
                instructions[copy.pc + 1] = value as u16;
                instructions[copy.pc + 2] = (value >> 16) as u16;
            }
            _ => return None,
        }
    }
    let mut regions = code.try_regions.clone();
    regions[plan.region_indices.0].end = plan.end as u32;
    regions.remove(plan.region_indices.1);
    Some(DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: method.thrown_types.clone(),
        access_flags: method.access_flags,
        code: Some(crate::native_dex::DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: u16::try_from(regions.len()).ok()?,
            try_regions: regions,
            instructions,
            offset: code.offset,
        }),
    })
}

pub(crate) fn prove(class: &DexClass, method: &DexMethod) -> Option<Plan> {
    let code = method.code.as_ref()?;
    if code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || !(2..=8).contains(&code.try_regions.len())
        || code.try_regions.iter().any(|r| r.catches.len() != 1)
    {
        return None;
    }
    let decoded = crate::native_ir::DecodedMethod::decode(code).ok()?;
    let ir = &decoded;
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let handler = |entry: usize| -> Option<Handler> {
        let first = at.get(&entry)?;
        if first.opcode != 0x0d || first.writes.len() != 1 {
            return None;
        }
        let caught = first.writes[0].register;
        let mut pc = entry + first.width;
        let mut copies = Vec::new();
        for _ in 0..8 {
            let i = at.get(&pc)?;
            if i.may_throw || i.payload_target.is_some() {
                return None;
            }
            match i.opcode {
                0x01..=0x03
                    if i.reads.len() == 1
                        && i.writes.len() == 1
                        && i.reads[0].kind.word_count() == 1
                        && i.writes[0].kind.word_count() == 1
                        && i.writes[0].register != caught =>
                {
                    copies.push(pc);
                    pc += i.width;
                }
                0x28..=0x2a => {
                    return Some(Handler {
                        entry,
                        caught,
                        copies,
                        last: pc,
                        common: i.branch_target?,
                    });
                }
                _ => return None,
            }
        }
        None
    };
    // Ordinary multi-catch methods stop here before CFG, SSA, or type inference.
    if !code.try_regions.windows(2).any(|pair| {
        let (first, second) = (&pair[0], &pair[1]);
        if first.start >= first.end
            || first.end > second.start
            || second.start >= second.end
            || first.catches[0].0 != second.catches[0].0
        {
            return false;
        }
        let (Some(a), Some(b)) = (
            handler(first.catches[0].1 as usize),
            handler(second.catches[0].1 as usize),
        ) else {
            return false;
        };
        a.caught == b.caught
            && a.common == b.common
            && !a.copies.is_empty()
            && a.copies.len() <= 4
            && b.copies.is_empty()
            && [&a, &b]
                .iter()
                .all(|h| (second.start as usize..second.end as usize).contains(&h.entry))
    }) {
        return None;
    }
    let analysis = MethodAnalysis::build(class, method).ok()?;
    if analysis.blocks().blocks.len() > 512 {
        return None;
    }
    let types = analysis.infer_types().ok()?;
    if types.wide_pair_issues != 0 {
        return None;
    }
    let cfg = analysis.blocks();
    let mut normal = BTreeSet::new();
    let mut pending = vec![*cfg.block_at.get(&0)?];
    while let Some(block) = pending.pop() {
        if !normal.insert(block) {
            continue;
        }
        pending.extend(
            cfg.blocks[block]
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::Normal)
                .map(|e| e.target),
        );
    }
    for index in 0..code.try_regions.len() - 1 {
        let (first, second) = (&code.try_regions[index], &code.try_regions[index + 1]);
        if first.start >= first.end
            || first.end > second.start
            || second.start >= second.end
            || first.catches[0].0 != second.catches[0].0
        {
            continue;
        }
        let (Some(a), Some(b)) = (
            handler(first.catches[0].1 as usize),
            handler(second.catches[0].1 as usize),
        ) else {
            continue;
        };
        if a.caught != b.caught
            || a.common != b.common
            || a.common < second.end as usize
            || !at.contains_key(&a.common)
            || a.copies.is_empty()
            || !b.copies.is_empty()
            || a.copies.len() > 4
            || [&a, &b].iter().any(|h| {
                !(second.start as usize..second.end as usize).contains(&h.entry)
                    || ir.instructions.iter().any(|i| {
                        (h.entry..=h.last).contains(&i.pc)
                            && cfg
                                .block_at
                                .get(&i.pc)
                                .is_some_and(|block| normal.contains(block))
                    })
                    || cfg.blocks.iter().enumerate().any(|(from, block)| {
                        block.successors.iter().any(|edge| {
                            edge.kind == EdgeKind::Normal
                                && (h.entry..=h.last).contains(&cfg.blocks[edge.target].start)
                                && !(h.entry..=h.last).contains(&cfg.blocks[from].start)
                        })
                    })
            })
            || code.try_regions.iter().enumerate().any(|(other, r)| {
                other != index
                    && other != index + 1
                    && (((r.start as usize) < second.end as usize
                        && (first.start as usize) < r.end as usize)
                        || r.catches.iter().any(|(_, target)| {
                            [&a, &b]
                                .iter()
                                .any(|h| (h.entry..=h.last).contains(&(*target as usize)))
                        }))
            })
            || ir.instructions.iter().any(|i| {
                (first.end as usize..second.start as usize).contains(&i.pc)
                    && (i.may_throw || i.opcode == 0x0d || i.payload_target.is_some())
            })
        {
            continue;
        }
        let ssa = analysis.ssa();
        let (Some(&common), Some(&a_from), Some(&b_from)) = (
            ssa.graph.block_at.get(&a.common),
            ssa.graph.block_at.get(&a.last),
            ssa.graph.block_at.get(&b.last),
        ) else {
            continue;
        };
        let mut constants = Vec::new();
        let mut destinations = BTreeSet::new();
        let mut valid = true;
        for &pc in &a.copies {
            let i = at[&pc];
            let register = i.writes[0].register;
            if !destinations.insert(register) {
                valid = false;
                break;
            }
            let phis: Vec<_> = ssa
                .phis
                .iter()
                .filter(|p| p.block == common && p.register == register)
                .collect();
            let [phi] = phis.as_slice() else {
                valid = false;
                break;
            };
            let incoming = |from| -> Option<usize> {
                let ids: Vec<_> = phi
                    .incoming
                    .iter()
                    .filter_map(|(source, id)| (*source == Some(from)).then_some(*id))
                    .collect();
                let [id] = ids.as_slice() else { return None };
                Some(*id)
            };
            let (Some(left), Some(right)) = (incoming(a_from), incoming(b_from)) else {
                valid = false;
                break;
            };
            let (Some(value), Some(other)) = (constant(&types, left), constant(&types, right))
            else {
                valid = false;
                break;
            };
            let writes: Vec<_> = ssa
                .instructions
                .iter()
                .filter(|s| s.pc == pc)
                .flat_map(|s| &s.writes)
                .filter(|w| w.register == register)
                .collect();
            let [write] = writes.as_slice() else {
                valid = false;
                break;
            };
            if value != other
                || write.words.len() != 1
                || constant(&types, write.words[0]) != Some(value)
                || !(match i.width {
                    1 => register < 16 && (-8..=7).contains(&value),
                    2 => (-32768..=32767).contains(&value),
                    3 => i32::try_from(value).is_ok(),
                    _ => false,
                })
            {
                valid = false;
                break;
            }
            constants.push(ConstantCopy {
                pc,
                register,
                literal: value,
                width: i.width,
            });
        }
        if valid && !constants.is_empty() {
            return Some(Plan {
                region_indices: (index, index + 1),
                start: first.start as usize,
                end: second.end as usize,
                selected_handler: a.entry,
                caught_register: a.caught,
                common: a.common,
                retired_prefix: b.entry..b.last + at[&b.last].width,
                constant_copies: constants,
                words: code.instructions.clone(),
                regions: code.try_regions.clone(),
                frame: (code.registers, code.ins, code.outs, code.tries),
            });
        }
    }
    None
}
