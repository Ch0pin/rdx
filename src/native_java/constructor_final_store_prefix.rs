//! Narrow constructor certificate for an own-final reference store at the
//! beginning of a typed `Exception` range. The DEX instruction stays in place;
//! only the effective Java protection start advances past the store.
use super::*;
use crate::{
    native_cfg::EdgeKind,
    native_dex::{DexCode, DexTryRegion},
    native_ir::PoolKind,
};
use std::collections::{BTreeMap, BTreeSet};

const EXCEPTION: &str = "Ljava/lang/Exception;";
const ERROR: &str = "Ljava/lang/Error;";
const THROWABLE: &str = "Ljava/lang/Throwable;";

/// Returns a complete adjusted method view, or no authority. Failed and
/// incomplete proofs deliberately fall back to the original strict path.
pub(super) fn adjust(class: &DexClass, method: &DexMethod) -> Option<DexMethod> {
    let regions = prove(class, method).ok().flatten()?;
    let code = method.code.as_ref()?;
    Some(DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: method.thrown_types.clone(),
        access_flags: method.access_flags,
        code: Some(DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: code.tries,
            try_regions: regions,
            instructions: code.instructions.clone(),
            offset: code.offset,
        }),
    })
}

fn prove(class: &DexClass, method: &DexMethod) -> Result<Option<Vec<DexTryRegion>>> {
    let code = method.code.as_ref().context("constructor prefix code")?;
    if method.name.as_ref() != "<init>" || method.access_flags & 8 != 0 {
        return Ok(None);
    }
    ensure!(
        code.instructions.len() <= 4096
            && code.registers <= 256
            && code.try_regions.len() <= 16
            && class.fields.len() <= 256
            && usize::from(code.tries) == code.try_regions.len()
            && code
                .try_regions
                .iter()
                .try_fold(0usize, |total, region| total
                    .checked_add(region.catches.len()))
                .is_some_and(|total| total <= 64),
        "constructor final-prefix budget"
    );
    if code.try_regions.is_empty() || !cheap_candidate(class, code) {
        return Ok(None);
    }
    ensure!(
        code.try_regions
            .windows(2)
            .all(|pair| pair[0].start < pair[0].end && pair[0].end <= pair[1].start)
            && code
                .try_regions
                .last()
                .is_none_or(|region| region.start < region.end),
        "constructor final-prefix regions overlap"
    );
    throwing::validate_catch_type(class, EXCEPTION, false)?;
    let hierarchy = class
        .symbols
        .hierarchy
        .get()
        .context("constructor final-prefix hierarchy unavailable")?;
    ensure!(
        [THROWABLE, EXCEPTION, ERROR]
            .into_iter()
            .all(|ty| hierarchy.unshadowed_sdk_type(ty))
            && hierarchy.assignable(EXCEPTION, THROWABLE)
                == crate::native_hierarchy::Relation::Proven
            && hierarchy.assignable(ERROR, THROWABLE) == crate::native_hierarchy::Relation::Proven
            && hierarchy.assignable(ERROR, EXCEPTION)
                == crate::native_hierarchy::Relation::Disproven
            && hierarchy.assignable(EXCEPTION, ERROR)
                == crate::native_hierarchy::Relation::Disproven,
        "constructor final-prefix platform exception identity unavailable"
    );

    let analysis = crate::native_method::MethodAnalysis::build(class, method)?;
    let ir = analysis.instructions();
    ensure!(
        !ir.instructions
            .iter()
            .any(|instruction| matches!(instruction.opcode, 0x1d | 0x1e)),
        "constructor final-prefix crosses monitor"
    );
    let at: BTreeMap<_, _> = ir
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let ssa_at: BTreeMap<_, _> = analysis
        .ssa()
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let cfg = analysis.blocks();

    let mut adjusted = code.try_regions.clone();
    let mut adjusted_regions = BTreeSet::new();
    let mut protected_own_finals = 0usize;
    for instruction in &ir.instructions {
        if !matches!(instruction.opcode, 0x59..=0x5f) {
            continue;
        }
        let covering: Vec<_> = code
            .try_regions
            .iter()
            .enumerate()
            .filter(|(_, region)| {
                region.start as usize <= instruction.pc && instruction.pc < region.end as usize
            })
            .collect();
        if covering.is_empty() {
            continue;
        }
        ensure!(
            covering.len() == 1,
            "constructor final-prefix multiple protection"
        );

        let pool = instruction
            .reference
            .as_ref()
            .context("constructor final-prefix field reference absent")?;
        ensure!(
            pool.kind == PoolKind::Field,
            "constructor final-prefix pool kind"
        );
        let &(owner_index, descriptor_index, name_index) = class
            .symbols
            .fields
            .get(pool.index as usize)
            .context("constructor final-prefix field index")?;
        let owner = class
            .symbols
            .types
            .get(owner_index as usize)
            .context("constructor final-prefix owner")?;
        if owner != &class.descriptor {
            continue;
        }
        let descriptor = class
            .symbols
            .types
            .get(descriptor_index as usize)
            .context("constructor final-prefix descriptor")?;
        let name = class
            .symbols
            .strings
            .get(name_index as usize)
            .context("constructor final-prefix name")?;
        let declarations: Vec<_> = class
            .fields
            .iter()
            .filter(|field| field.name.as_ref() == name.as_str())
            .collect();
        ensure!(
            declarations.len() == 1,
            "constructor final-prefix ambiguous own field"
        );
        let field = declarations[0];
        let visibility = field.access_flags & 7;
        ensure!(
            field.declaring_type == class.descriptor
                && field.field_type == *descriptor
                && field.access_flags & !0x10df == 0
                && matches!(visibility, 0 | 1 | 2 | 4)
                && java_type(&field.field_type).is_ok(),
            "constructor final-prefix declaration mismatch"
        );
        if field.access_flags & 0x10 == 0 {
            continue;
        }
        protected_own_finals = protected_own_finals
            .checked_add(1)
            .context("constructor final-prefix count overflow")?;
        ensure!(
            instruction.opcode == 0x5b
                && reference(&field.field_type)
                && !field.is_static
                && field.access_flags & 8 == 0
                && field.access_flags & 0x40 == 0,
            "constructor final-prefix field is not exact final reference"
        );

        let (region_index, region) = covering[0];
        ensure!(
            instruction.pc == region.start as usize
                && adjusted_regions.insert(region_index)
                && region.catches.len() == 1
                && region.catches[0].0.as_deref() == Some(EXCEPTION),
            "constructor final-prefix range shape"
        );
        let next = instruction
            .pc
            .checked_add(instruction.width)
            .context("constructor final-prefix endpoint overflow")?;
        ensure!(
            instruction.width == 2
                && next < region.end as usize
                && at.contains_key(&next)
                && region.catches[0].1 >= region.end
                && at
                    .get(&(region.catches[0].1 as usize))
                    .is_some_and(|handler| handler.opcode == 0x0d),
            "constructor final-prefix boundary or handler"
        );

        let receiver = ssa_at
            .get(&instruction.pc)
            .and_then(|ssa| ssa.reads.first())
            .context("constructor final-prefix receiver absent")?;
        ensure!(
            receiver.words.len() == 1
                && direct_this_copy_value(method, &analysis, receiver.words[0]),
            "constructor final-prefix receiver is not original this"
        );

        let store_block = *cfg
            .block_at
            .get(&instruction.pc)
            .context("constructor final-prefix store block")?;
        let next_block = *cfg
            .block_at
            .get(&next)
            .context("constructor final-prefix next block")?;
        let store_incoming: Vec<_> = cfg
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(source, block)| {
                block
                    .successors
                    .iter()
                    .filter(move |edge| edge.target == store_block)
                    .map(move |edge| (source, edge.kind))
            })
            .collect();
        let [(prefix_block, EdgeKind::Normal)] = store_incoming.as_slice() else {
            bail!("constructor final-prefix branch entry");
        };
        let prefix_pc = *cfg.blocks[*prefix_block]
            .instructions
            .last()
            .context("constructor final-prefix predecessor absent")?;
        let prefix_instruction = at
            .get(&prefix_pc)
            .context("constructor final-prefix predecessor instruction absent")?;
        ensure!(
            store_block != next_block
                && prefix_instruction.pc + prefix_instruction.width == instruction.pc
                && prefix_instruction.branch_target.is_none()
                && prefix_instruction.payload_target.is_none()
                && cfg.blocks[store_block].instructions.as_slice() == [instruction.pc]
                && cfg.blocks[store_block].successors.iter().any(|edge| {
                    edge.kind == EdgeKind::Exceptional
                        && cfg.blocks[edge.target].start == region.catches[0].1 as usize
                }),
            "constructor final-prefix store block shape"
        );
        let incoming: Vec<_> = cfg
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(source, block)| {
                block
                    .successors
                    .iter()
                    .filter(move |edge| edge.target == next_block)
                    .map(move |edge| (source, edge.kind))
            })
            .collect();
        ensure!(
            incoming.as_slice() == [(store_block, EdgeKind::Normal)],
            "constructor final-prefix alternate protected entry"
        );
        adjusted[region_index].start =
            u32::try_from(next).context("constructor final-prefix adjusted start overflow")?;
    }

    if protected_own_finals == 0 {
        return Ok(None);
    }
    ensure!(
        protected_own_finals == adjusted_regions.len(),
        "constructor final-prefix plan is partial"
    );
    Ok(Some(adjusted))
}

/// Allocation-free prefilter. False positives enter the complete proof; raw
/// words only avoid CFG/SSA construction for ordinary constructors.
fn cheap_candidate(class: &DexClass, code: &DexCode) -> bool {
    code.try_regions.iter().any(|region| {
        if region.catches.len() != 1 || region.catches[0].0.as_deref() != Some(EXCEPTION) {
            return false;
        }
        let start = region.start as usize;
        let Some((&opcode, &field_index)) = code
            .instructions
            .get(start)
            .zip(code.instructions.get(start.saturating_add(1)))
        else {
            return false;
        };
        if opcode as u8 != 0x5b {
            return false;
        }
        let Some(&(owner_index, descriptor_index, name_index)) =
            class.symbols.fields.get(field_index as usize)
        else {
            return false;
        };
        let (Some(owner), Some(descriptor), Some(name)) = (
            class.symbols.types.get(owner_index as usize),
            class.symbols.types.get(descriptor_index as usize),
            class.symbols.strings.get(name_index as usize),
        ) else {
            return false;
        };
        owner == &class.descriptor
            && reference(descriptor)
            && class.fields.iter().any(|field| {
                field.declaring_type == class.descriptor
                    && field.name.as_ref() == name.as_str()
                    && field.field_type == *descriptor
                    && field.access_flags & 0x10 != 0
                    && !field.is_static
            })
    })
}
