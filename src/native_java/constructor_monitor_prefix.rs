//! First constructor monitor-prefix tranche: one static lock, straight-line
//! primitive/static effects, exact release/rethrow, then Object initialization.
//! This certificate opens only the protection-position and matching monitor-entry
//! gates. Existing continuation/final-field proofs remain mandatory.
use super::*;
use crate::{native_cfg::EdgeKind, native_ir::ValueKind, native_ssa::DefinitionKind};
use std::collections::BTreeMap;

pub(super) fn prove(
    class: &DexClass,
    method: &DexMethod,
    analysis: &crate::native_method::MethodAnalysis<'_>,
    initializer_pc: usize,
    initializer_end: usize,
) -> bool {
    let check = || -> Result<()> {
        let code = method.code.as_ref().context("monitor prefix code")?;
        ensure!(
            (6..=64).contains(&initializer_pc)
                && code.ins > 0
                && code.ins <= code.registers
                && code.tries == 1
                && code.try_regions.len() == 1,
            "monitor prefix shape budget"
        );
        let ir = analysis.instructions();
        let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let prefix: Vec<_> = ir
            .instructions
            .iter()
            .filter(|i| i.pc < initializer_pc)
            .collect();
        ensure!(
            (4..=32).contains(&prefix.len())
                && prefix[0].pc == 0
                && prefix[0].opcode == 0x62
                && prefix[0].width == 2
                && prefix[1].pc == 2
                && prefix[1].opcode == 0x1d
                && prefix[1].width == 1
                && ir.instructions.iter().filter(|i| i.opcode == 0x1d).count() == 1
                && ir.instructions.iter().filter(|i| i.opcode == 0x1e).count() == 2,
            "monitor prefix requires one initial static lock"
        );
        let region = &code.try_regions[0];
        ensure!(
            region.start == 3
                && region.end as usize + 1 == initializer_pc
                && region.catches.len() == 1
                && region.catches[0].0.is_none(),
            "monitor prefix protection boundaries"
        );
        let normal_release = at
            .get(&(region.end as usize))
            .context("normal release boundary")?;
        ensure!(
            normal_release.opcode == 0x1e && normal_release.width == 1,
            "normal release immediately precedes initializer"
        );
        let handler = region.catches[0].1 as usize;
        ensure!(
            handler >= initializer_end && handler.checked_add(3) == Some(code.instructions.len()),
            "monitor prefix has terminal three-instruction cleanup"
        );
        let caught = at.get(&handler).context("caught exception boundary")?;
        let exceptional_release = at
            .get(&(handler + 1))
            .context("exceptional release boundary")?;
        let rethrow = at.get(&(handler + 2)).context("rethrow boundary")?;
        ensure!(
            caught.opcode == 0x0d
                && exceptional_release.opcode == 0x1e
                && rethrow.opcode == 0x27
                && [caught.width, exceptional_release.width, rethrow.width] == [1, 1, 1],
            "monitor prefix requires exact capture/release/rethrow"
        );
        for pair in prefix.windows(2) {
            ensure!(
                pair[0].pc + pair[0].width == pair[1].pc,
                "monitor prefix instruction gap"
            );
        }
        ensure!(
            prefix
                .last()
                .is_some_and(|i| i.pc + i.width == initializer_pc),
            "monitor prefix does not reach initializer"
        );
        for i in prefix.iter().skip(2).filter(|i| i.pc < region.end as usize) {
            ensure!(
                (matches!(i.opcode, 0x01..=0x09 | 0x12..=0x1c | 0x2d..=0x31 | 0x60..=0x6d | 0x7b..=0xe2)
                    || i.opcode == 0 && i.width == 1)
                    && i.branch_target.is_none()
                    && i.payload_target.is_none(),
                "monitor prefix body is not static/primitive straight-line flow"
            );
        }
        // Own declarations give exact accessibility/type/static authority. A
        // qualified access within its declaring class can use every valid
        // visibility; inherited or external fields require another tranche.
        for instruction in prefix.iter().filter(|i| matches!(i.opcode, 0x60..=0x6d)) {
            let pool = instruction
                .reference
                .context("static field reference absent")?;
            ensure!(
                pool.kind == crate::native_ir::PoolKind::Field,
                "static field pool kind"
            );
            let &(owner, ty, name) = class
                .symbols
                .fields
                .get(pool.index as usize)
                .context("static field index")?;
            let owner = class
                .symbols
                .types
                .get(owner as usize)
                .context("static field owner")?;
            let ty = class
                .symbols
                .types
                .get(ty as usize)
                .context("static field type")?;
            let name = class
                .symbols
                .strings
                .get(name as usize)
                .context("static field name")?;
            ensure!(owner == &class.descriptor, "external prefix static field");
            let mut declarations = class
                .fields
                .iter()
                .filter(|field| field.name.as_ref() == name.as_str());
            let field = declarations
                .next()
                .context("prefix static declaration absent")?;
            let visibility = field.access_flags & 7;
            ensure!(
                declarations.next().is_none()
                    && field.declaring_type == class.descriptor
                    && &field.field_type == ty
                    && field.is_static
                    && field.access_flags & 8 != 0
                    && field.access_flags & !0x10df == 0
                    && matches!(visibility, 0 | 1 | 2 | 4)
                    && field.access_flags & 0x50 != 0x50
                    && (instruction.opcode < 0x67 || field.access_flags & 0x10 == 0),
                "prefix static field declaration/access/writability mismatch"
            );
            let family = instruction.opcode
                - if instruction.opcode >= 0x67 {
                    0x67
                } else {
                    0x60
                };
            ensure!(
                match family {
                    0 => matches!(ty.as_ref(), "I" | "F"),
                    1 => matches!(ty.as_ref(), "J" | "D"),
                    2 => reference(ty),
                    3 => ty.as_ref() == "Z",
                    4 => ty.as_ref() == "B",
                    5 => ty.as_ref() == "C",
                    6 => ty.as_ref() == "S",
                    _ => false,
                },
                "prefix static opcode type mismatch"
            );
        }
        let ssa = analysis.ssa();
        let reads: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let read = |pc: usize| -> Result<usize> {
            let instruction = reads.get(&pc).context("monitor SSA read absent")?;
            ensure!(instruction.reads.len() == 1, "monitor SSA read count");
            let value = &instruction.reads[0];
            ensure!(
                value.kind == ValueKind::Reference && value.words.len() == 1,
                "monitor SSA read width"
            );
            Ok(value.words[0])
        };
        let write = |pc: usize| -> Result<usize> {
            let instruction = reads.get(&pc).context("monitor SSA write absent")?;
            ensure!(instruction.writes.len() == 1, "monitor SSA write count");
            let value = &instruction.writes[0];
            ensure!(
                value.kind == ValueKind::Reference && value.words.len() == 1,
                "monitor SSA write width"
            );
            Ok(value.words[0])
        };
        let lock = read(2)?;
        ensure!(
            lock == write(0)?
                && lock == read(normal_release.pc)?
                && lock == read(exceptional_release.pc)?
                && matches!(
                    ssa.definitions.get(lock).map(|d| &d.kind),
                    Some(DefinitionKind::Instruction { pc: 0, word: 0, .. })
                ),
            "monitor prefix lock identity differs"
        );
        let exception = write(handler)?;
        ensure!(
            exception == read(handler + 2)?
                && exception != lock
                && matches!(ssa.definitions.get(exception).map(|d| &d.kind), Some(DefinitionKind::Instruction { pc, word: 0, .. }) if *pc == handler),
            "monitor prefix exception identity differs"
        );
        // Terminal cleanup may overwrite the former receiver register with the
        // caught exception: the exact SSA capture/release/rethrow proof above
        // establishes that no read there observes the incoming receiver value.
        // Normal prefix instructions must leave incoming this untouched.
        let receiver = usize::from(code.registers - code.ins);
        for instruction in prefix.iter().map(|i| i.pc) {
            let values = reads
                .get(&instruction)
                .context("monitor prefix SSA instruction absent")?;
            ensure!(
                values.reads.iter().chain(&values.writes).all(|operand| {
                    let first = usize::from(operand.register);
                    receiver < first || receiver >= first + operand.words.len()
                }),
                "monitor prefix uses or overwrites incoming this"
            );
        }
        // Prove entry ownership from the original CFG, before SSA introduces
        // normal-success write blocks. External branches cannot enter either
        // the prefix/initializer or the middle of its terminal cleanup.
        let cfg = analysis.blocks();
        for block in &cfg.blocks {
            for edge in &block.successors {
                let target = cfg.blocks[edge.target].start;
                let source = *block
                    .instructions
                    .last()
                    .context("monitor prefix empty block")?;
                if target <= initializer_pc {
                    let instruction = at.get(&source).context("monitor prefix edge source")?;
                    ensure!(
                        edge.kind == EdgeKind::Normal
                            && source < initializer_pc
                            && source + instruction.width == target,
                        "alternate constructor prefix entry"
                    );
                }
                if (handler..handler + 3).contains(&target) {
                    ensure!(
                        if edge.kind == EdgeKind::Exceptional {
                            target == handler
                                && block.instructions.iter().all(|pc| {
                                    region.start as usize <= *pc && *pc < region.end as usize
                                })
                        } else {
                            (handler..handler + 2).contains(&source) && source + 1 == target
                        },
                        "alternate monitor cleanup entry"
                    );
                }
            }
        }
        Ok(())
    };
    check().is_ok()
}
